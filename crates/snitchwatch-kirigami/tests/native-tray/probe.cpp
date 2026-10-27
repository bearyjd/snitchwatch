#include <QApplication>
#include <QElapsedTimer>
#include <QEvent>
#include <QMenu>
#include <QQmlComponent>
#include <QQmlEngine>
#include <QThread>
#include <QWindow>
#include <QtDBus>
#include <private/qguiapplication_p.h>
#include <qpa/qplatformtheme.h>
#include <memory>
#include <typeinfo>
#include <cstdio>

// A typed boundary for the real TrayMenu.qml. The Rust tray_menu_qml test
// separately checks these calls against real bridge messages.
class TrayProbeController : public QObject {
    Q_OBJECT
    Q_PROPERTY(QString menuLabel MEMBER menuLabel NOTIFY stateChanged)
    Q_PROPERTY(QString pausedUntil MEMBER pausedUntil NOTIFY stateChanged)
    Q_PROPERTY(bool pauseAnswersWaiting MEMBER pauseAnswersWaiting NOTIFY stateChanged)
public:
    QString menuLabel = QStringLiteral("pause_filtering");
    QString pausedUntil;
    bool pauseAnswersWaiting = true;
    QList<int> pauses;
    int resumes = 0;
    Q_INVOKABLE void pauseFor(int seconds) { pauses.append(seconds); }
    Q_INVOKABLE void resume() { ++resumes; }
signals:
    void stateChanged();
};

class TrayProbeWatcher : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.kde.StatusNotifierWatcher")
    Q_PROPERTY(bool IsStatusNotifierHostRegistered READ hasHost)
    Q_PROPERTY(int ProtocolVersion READ version)
    Q_PROPERTY(QStringList RegisteredStatusNotifierItems READ items)
public:
    QString itemService;
    bool hasHost() const { return true; }
    int version() const { return 0; }
    QStringList items() const { return {}; }
public slots:
    void RegisterStatusNotifierItem(const QString &service) { itemService = service; }
    void RegisterStatusNotifierHost(const QString &) {}
};

class TrayProbeShowFilter : public QObject {
public:
    int shows = 0;
    bool eventFilter(QObject *object, QEvent *event) override {
        if (event->type() == QEvent::Show && object->inherits("QMenu"))
            ++shows;
        return false;
    }
};

static void require(bool ok, const QString &message) {
    if (!ok)
        throw message;
}

static QDBusMessage awaitReply(const QDBusMessage &request) {
    auto pending = QDBusConnection::sessionBus().asyncCall(request, 3000);
    QElapsedTimer deadline;
    deadline.start();
    while (!pending.isFinished() && deadline.elapsed() < 4000) {
        QApplication::processEvents();
        QThread::msleep(1);
    }
    require(pending.isFinished(), QStringLiteral("private D-Bus request timed out"));
    const QDBusMessage reply = pending.reply();
    require(reply.type() == QDBusMessage::ReplyMessage, reply.errorMessage());
    return reply;
}

static void collectLabels(const QDBusArgument &layout, QMap<QString, int> &labels) {
    int id;
    QVariantMap properties;
    layout.beginStructure();
    layout >> id >> properties;
    if (properties.contains(QStringLiteral("label")))
        labels.insert(properties.value(QStringLiteral("label")).toString(), id);
    layout.beginArray();
    while (!layout.atEnd()) {
        QDBusVariant child;
        layout >> child;
        collectLabels(qvariant_cast<QDBusArgument>(child.variant()), labels);
    }
    layout.endArray();
    layout.endStructure();
}

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    try {
        require(argc == 2, QStringLiteral("pass the absolute source qml directory"));
        // Reject fallback backends: the generic widget fallback does not have
        // KDE SystemTrayMenu's visibility behavior and would false-pass.
        const QString theme = QString::fromLatin1(typeid(*QGuiApplicationPrivate::platformTheme()).name());
        require(theme.contains(QStringLiteral("KdePlatformTheme")), QStringLiteral("KDE theme required: ") + theme);
        std::printf("Qt=%s platform=%s theme=%s\n", qVersion(), qPrintable(QGuiApplication::platformName()), qPrintable(theme));
        TrayProbeShowFilter filter;
        app.installEventFilter(&filter);
        TrayProbeWatcher watcher;
        auto bus = QDBusConnection::sessionBus();
        require(bus.registerService(QStringLiteral("org.kde.StatusNotifierWatcher")), bus.lastError().message());
        require(bus.registerObject(QStringLiteral("/StatusNotifierWatcher"), &watcher,
            QDBusConnection::ExportAllSlots | QDBusConnection::ExportAllProperties), bus.lastError().message());
        qmlRegisterType<TrayProbeController>("com.snitchwatch.shell", 1, 0, "TrayController");
        QQmlEngine engine;
        QQmlComponent component(&engine);
        QByteArray qml = "import QtQuick\nimport Qt.labs.platform as Labs\nimport com.snitchwatch.shell\nimport \"";
        qml += QUrl::fromLocalFile(QString::fromLocal8Bit(argv[1])).toString().toUtf8();
        qml += "\" as App\nLabs.SystemTrayIcon { visible: true; menu: App.TrayMenu { controller: TrayController { objectName: \"controllerProbe\" } window: QtObject { objectName: \"windowProbe\"; property bool visible: true; function hide() { visible = false; } function raiseAndActivate() { visible = true; } } } }";
        component.setData(qml, QUrl(QStringLiteral("file:///tmp/tray-probe.qml")));
        std::unique_ptr<QObject> tray(component.create());
        require(bool(tray), component.errorString());
        QElapsedTimer registered;
        registered.start();
        while (watcher.itemService.isEmpty() && registered.elapsed() < 3000) {
            QApplication::processEvents();
            QThread::msleep(1);
        }
        require(!watcher.itemService.isEmpty(), QStringLiteral("KDE tray never registered with private watcher"));
        QMenu *menu = nullptr;
        for (QWidget *widget : QApplication::allWidgets()) {
            if (auto *candidate = qobject_cast<QMenu *>(widget)) {
                require(!menu, QStringLiteral("unexpected extra native menu"));
                menu = candidate;
            }
        }
        require(menu, QStringLiteral("KDE native QMenu missing"));
        const int spontaneousShows = filter.shows;
        const bool initiallyVisible = menu->isVisible();
        std::printf("startup: spontaneous-shows=%d visible=%d native-actions=%lld\n", spontaneousShows, initiallyVisible, static_cast<long long>(menu->actions().size()));
        require(menu->actions().size() == 8, QStringLiteral("native tray actions changed"));

        auto getLayout = QDBusMessage::createMethodCall(watcher.itemService,
            QStringLiteral("/MenuBar"), QStringLiteral("com.canonical.dbusmenu"), QStringLiteral("GetLayout"));
        getLayout.setArguments({0, -1, QStringList{}});
        const auto layoutReply = awaitReply(getLayout);
        require(layoutReply.arguments().size() == 2, QStringLiteral("exported menu layout missing"));
        QMap<QString, int> labels;
        collectLabels(qvariant_cast<QDBusArgument>(layoutReply.arguments().at(1)), labels);
        require(labels.size() == 6, QStringLiteral("exported tray entries changed: ") + labels.keys().join(QStringLiteral(", ")));
        require(labels.contains(QStringLiteral("Quit")), QStringLiteral("exported Quit action missing"));
        auto click = [&](const QString &label) {
            require(labels.contains(label), QStringLiteral("exported action missing: ") + label);
            auto event = QDBusMessage::createMethodCall(watcher.itemService, QStringLiteral("/MenuBar"),
                QStringLiteral("com.canonical.dbusmenu"), QStringLiteral("Event"));
            event.setArguments({labels.value(label), QStringLiteral("clicked"), QVariant::fromValue(QDBusVariant(0)), 0u});
            awaitReply(event);
            QApplication::processEvents();
        };
        auto *controller = tray->findChild<TrayProbeController *>(QStringLiteral("controllerProbe"));
        require(controller, QStringLiteral("typed tray controller missing"));
        for (const auto &[text, seconds] : QList<QPair<QString, int>>{
            {QStringLiteral("Pause for 5 minutes"), 300}, {QStringLiteral("Pause for 30 minutes"), 1800},
            {QStringLiteral("Pause for 1 hour"), 3600}}) {
            click(text + QStringLiteral(" (also lets waiting connections through once)"));
            require(!controller->pauses.isEmpty() && controller->pauses.last() == seconds,
                QStringLiteral("exported pause action misrouted"));
        }
        controller->menuLabel = QStringLiteral("resume_filtering");
        emit controller->stateChanged();
        QApplication::processEvents();
        click(QStringLiteral("Resume prompting"));
        require(controller->resumes == 1, QStringLiteral("exported resume action did not run"));
        click(QStringLiteral("Hide window"));
        auto *window = tray->findChild<QObject *>(QStringLiteral("windowProbe"));
        require(window && !window->property("visible").toBool(), QStringLiteral("exported hide action did not run"));
        labels.clear();
        collectLabels(qvariant_cast<QDBusArgument>(awaitReply(getLayout).arguments().at(1)), labels);
        click(QStringLiteral("Show window"));
        require(window->property("visible").toBool(), QStringLiteral("exported show action did not run"));

        // Show it explicitly using the native popup API, independently of its
        // initialization visibility. A real compositor/input check is separate.
        menu->hide();
        const int beforeManual = filter.shows;
        menu->popup(QPoint(12, 12));
        QApplication::processEvents();
        require(menu->isVisible() && filter.shows == beforeManual + 1,
            QStringLiteral("explicit native context menu no longer opens"));
        menu->hide();
        std::printf("export: six entries; D-Bus pause/resume/show-hide actions and explicit native popup PASS\n");
        require(!initiallyVisible && spontaneousShows == 0,
            QStringLiteral("FAIL: KDE tray menu showed before any user interaction"));
        std::puts("native tray startup PASS");
        return 0;
    } catch (const QString &error) {
        std::fprintf(stderr, "%s\n", qPrintable(error));
        return 1;
    }
}

#include "probe.moc"
