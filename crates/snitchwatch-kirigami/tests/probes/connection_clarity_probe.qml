import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000
    height: 1100
    property var failures: []
    property int step: 0
    property string path: "/opt/<b>my app</b>/bin/run & go"
    property string host: "<img src=x> & host.example"
    property string ip: "2001:db8::cafe"
    property var snapshot: null
    QtObject {
        id: feed
        property bool ok: true
        function submitVerdict() { return true; }
    }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feed
        model: ConnectionsModel { id: rows }
    }
    QtObject {
        id: commands
        property string session: "1:"
        property string duration: "for_5_minutes"
        property var calls: []
        signal modelReset()
        signal levelRuleFinished(int level, string program, string key, int ending, string status)
        function currentSourceSession() { return session; }
        function levelRuleDuration() { return duration; }
        function levelRuleScopeText(level, program, key) { return rows.levelRuleScopeText(level, program, key); }
        function createLevelRuleForSession(level, program, key, choice, duration, session) {
            calls = calls.concat([[level, program, key, choice, duration, session]]);
            return "";
        }
        function pollLevelRules() {}
    }
    LevelRuleActions { id: rules; model: commands; width: 600 }
    Controls.TextArea { id: pasted; visible: false; textFormat: TextEdit.PlainText }
    TestCase { id: tc; name: "clarity"; when: false }
    function expect(ok, why) { if (!ok) failures.push(why); }
    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        for (const kid of item.children || []) {
            const hit = find(kid, name); if (hit) return hit;
        }
        return null;
    }
    function row(id, p, h, ip, port) {
        return { id: id, process: "friendly label", processPath: p, dstHost: h,
            dstIp: ip, dstPort: port, protocol: "tcp", direction: "outgoing",
            action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1,
            matchedRule: "<b>recorded-rule</b>" };
    }
    function delegate(id) {
        for (let i=0; i<page.connectionList.count; ++i) {
            const d=page.connectionList.itemAtIndex(i); if (d && d.rowId === id) return d;
        }
        throw new Error("missing delegate " + id);
    }
    function checkCopy(name, value, root) {
        const button = find(root || page, name);
        expect(!!button, "missing " + name);
        if (!button) return;
        expect(button.enabled, "disabled " + name);
        if (typeof button.trigger === "function") button.trigger();
        else button.clicked();
        pasted.text = ""; pasted.paste();
        expect(pasted.text === value, name + " copied " + pasted.text);
    }
    readonly property var steps: [
        function () {
            rows.setGroupedMode(false);
            rows.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [row("a", path, host, ip, 443)] }));
        },
        function () {
            tc.mouseClick(delegate("a"), 2, 2);
            const explanation=find(page,"connectionExplanation");
            expect(!!explanation, "missing factual explanation");
            if (explanation) {
                expect(explanation.textFormat === Text.PlainText, "explanation renders markup");
                for (const value of [path, host, ip, "443", "tcp", "recorded-rule", "recorded", "current policy"])
                    expect(explanation.text.indexOf(value)>=0, "explanation missing " + value + ": " + explanation.text);
                expect(explanation.text.indexOf("friendly label")<0, "display name presented as executable");
            }
            checkCopy("copyExecutable", path);
            checkCopy("copyHost", host);
            checkCopy("copyIp", ip);
            checkCopy("copyPort", "443");
            snapshot=page.connectionActions.snapshot(delegate("a"));
            page.connectionActions.show(snapshot,page,0,0,false);
        },
        function () {
            // Reordered/recreated delegates must not alter a captured menu.
            rows.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [row("b", "/different", "different", "192.0.2.1", 80)] }));
        },
        function () {
            checkCopy("copyExecutable", path, page.connectionActions.menu.contentItem);
            checkCopy("copyHost", host, page.connectionActions.menu.contentItem);
            checkCopy("copyIp", ip, page.connectionActions.menu.contentItem);
            checkCopy("copyPort", "443", page.connectionActions.menu.contentItem);
            page.connectionActions.close();
            tc.mouseClick(delegate("b"),2,2);
            feed.ok=false;
            const copy=find(page,"copyExecutable");
            expect(copy && !copy.enabled,"stale inspector clipboard remains enabled");
            feed.ok=true;
            rows.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [row("missing",null,"","",0)] }));
        },
        function () {
            tc.mouseClick(delegate("missing"),2,2);
            for (const name of ["copyExecutable","copyHost","copyIp","copyPort"]) {
                const button=find(page,name); expect(button && !button.enabled,"missing metadata enabled " + name);
            }
            const explanation=find(page,"connectionExplanation");
            expect(explanation && explanation.text.indexOf("unknown")>=0,"missing metadata not unknown");
            rules.create(0,path,"","friendly label","allow","1:");
            expect(rules.confirmText.indexOf("any port")>=0,"app preview lacks any port");
            expect(rules.confirmText.indexOf(path)>=0,"app preview lacks actual program");
            expect(rules.confirmText.indexOf("5 minutes")>=0,"app preview lacks dispatched duration");
            rules.confirmDialog.close();
        },
        function () {
            expect(commands.calls.length===0,"cancel dispatched");
            rules.create(0,path,"","label","allow","1:");
            commands.duration="forever";
            rules.confirmDialog.customFooterActions[0].trigger();
            expect(commands.calls.length===0,"changed duration dispatched from old preview");
        },
        function () {
            rows.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [row("zero","0","0","0",443)] }));
        },
        function () {
            tc.mouseClick(delegate("zero"),2,2);
            checkCopy("copyExecutable","0");
            checkCopy("copyHost","0");
            checkCopy("copyIp","0");
        },
        function () {
            for (const item of [[0,"","any host"],[1,"example.com","domain"],[2,"api.example.com","exact host"],[1,"2001:db8::1","exact IP"],[1,"localhost","exact host"]]) {
                const text=rows.levelRuleScopeText(item[0],path,item[1]);
                expect(text.indexOf(item[2])>=0 && text.indexOf("any port")>=0 && text.indexOf(path)>=0,"wrong scope " + JSON.stringify(item) + ": " + text);
            }
            expect(rows.levelRuleScopeText(1,path,"co.uk").indexOf("unavailable")>=0,"public suffix preview advertised domain");
            expect(rows.levelRuleScopeText(2,"unknown","a.example.com").indexOf("unavailable")>=0,"missing binding advertised rule");
        },
        function () {
            commands.duration="for_5_minutes";
            rules.create(0,path,"","label","deny","1:");
            rules.confirmDialog.customFooterActions[0].trigger();
            expect(JSON.stringify(commands.calls)===JSON.stringify([[0,path,"","deny","for_5_minutes","1:"]]),"accept payload differs from preview");
            rules.create(0,path,"","label","allow","1:");
            commands.session="2:"; commands.modelReset();
            rules.accepted();
            expect(commands.calls.length===1,"replacement session dispatched");
            const pending=row("pending","/pending","pending.example","",443); pending.action=null;
            rows.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [pending] }));
        },
        function () {
            tc.mouseClick(delegate("pending"),2,2);
            checkCopy("copyPort","443");
            rows.applyServerMessageJson(JSON.stringify({ action: "clearConnectionRows" }));
        },
        function () {
            const copy=find(page,"copyExecutable");
            expect(page.inspectNoLongerPending,"withdrawn pending inspector did not remain");
            expect(copy && copy.enabled,"historical same-session copy disabled");
            checkCopy("copyExecutable","/pending");
            checkCopy("copyPort","443");
            page.inspectSession="replacement:";
            expect(copy && !copy.enabled,"replacement session inspector copy enabled");
            if(copy) copy.clicked();
            pasted.text=""; pasted.paste();
            expect(pasted.text==="443","replacement session copied stale bytes: " + pasted.text);
            page.inspectSession="";
            checkCopy("copyExecutable","/pending");
        }
    ]
    Timer {
        interval: 250
        running: true
        repeat: true
        onTriggered: {
            if (step >= steps.length) {
                stop();
                if (failures.length) console.error("connection clarity failures: " + failures.join("; "));
                else console.warn("CONNECTION_CLARITY_PROBE_DONE");
                Qt.quit(); return;
            }
            try { steps[step](); } catch(e) { failures.push("step " + step + ": " + e); }
            ++step;
        }
    }
}
