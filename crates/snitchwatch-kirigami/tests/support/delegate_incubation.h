#pragma once

#include <QtCore/QTimer>
#include <QtQml/QQmlApplicationEngine>
#include <QtQml/QQmlIncubationController>
#include <algorithm>
#include <memory>

namespace snitchwatch::test {

// A slow frame can leave a cache delegate incubating across a valid model
// reset. Hold that work deterministically, using only public Qt APIs.
class IncubationHold {
public:
    explicit IncubationHold(QQmlApplicationEngine &engine)
        : engine_(engine), previous_(engine.incubationController())
    {
        engine_.setIncubationController(&controller_);
        QObject::connect(&timer_, &QTimer::timeout, [this] {
            maximum_ = std::max(maximum_, controller_.incubatingObjectCount());
        });
        timer_.start(1);
    }

    ~IncubationHold()
    {
        timer_.stop();
        engine_.setIncubationController(previous_);
    }

    int maxPending() const { return maximum_; }

private:
    QQmlApplicationEngine &engine_;
    QQmlIncubationController *previous_;
    QQmlIncubationController controller_;
    QTimer timer_;
    int maximum_ = 0;
};

inline std::unique_ptr<IncubationHold> holdIncubation(QQmlApplicationEngine &engine)
{
    return std::make_unique<IncubationHold>(engine);
}

} // namespace snitchwatch::test
