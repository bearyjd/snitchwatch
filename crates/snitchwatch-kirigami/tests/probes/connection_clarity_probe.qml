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
        property bool appBound: true
        property var submitted: []
        function appBoundRulesFor(id) { return appBound && id !== "unknown"; }
        function submitVerdict(id, choice, scope, duration, bound) {
            submitted = submitted.concat([[id, choice, scope, duration, bound]]);
            return true;
        }
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
                for (const value of [path, host, ip, "443", "tcp", "recorded-rule", "recorded", "Current rules may differ"])
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
            const recorded=find(page,"connectionExplanation");
            expect(recorded && recorded.text.indexOf("Recorded verdict: " + page.inspectVerdict + ".")>=0,
                   "lifecycle status mislabeled as recorded verdict: " + (recorded ? recorded.text : "missing"));
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
            Durations.defaultToken="for_5_minutes";
            page.dismissInspector();
            const remembered=row("decision",path,"api.example.com",ip,443); remembered.action=null;
            rows.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [remembered] }));
        },
        function () {
            const target=page.connectionActions.snapshot(delegate("decision"));
            page.connectionActions.show(target,page,0,0,false);
            const allow=find(page.connectionActions.menu.contentItem,"allowPendingAction");
            const deny=find(page.connectionActions.menu.contentItem,"denyPendingAction");
            expect(allow && allow.text.indexOf("5 minutes")>=0,"Allow preview omits actual remembered duration: " + (allow ? allow.text : "missing"));
            expect(deny && deny.text.indexOf("Until firewall restarts")>=0,"Deny preview omits actual remembered duration: " + (deny ? deny.text : "missing"));
            const scope=find(page.connectionActions.menu.contentItem,"connectionScopePreview");
            expect(scope && scope.text.indexOf("no remembered rule")<0,"preview falsely promises no remembered rule");
            page.connectionActions.decide("allow");
            expect(JSON.stringify(feed.submitted[feed.submitted.length-1])===JSON.stringify(["decision","allow","this_host","for_5_minutes",true]),"production Allow payload unexpected: " + JSON.stringify(feed.submitted));
            page.inlineVerdicts.submittedIds=({});
            page.connectionActions.show(target,page,0,0,false);
            page.connectionActions.decide("deny");
            expect(JSON.stringify(feed.submitted[feed.submitted.length-1])===JSON.stringify(["decision","deny","this_host","until_restart",true]),"production Deny payload unexpected: " + JSON.stringify(feed.submitted));
            page.inlineVerdicts.submittedIds=({});
            page.connectionActions.show(target,page,0,0,false);
            const before=feed.submitted.length;
            feed.appBound=false;
            page.connectionActions.decide("allow");
            expect(feed.submitted.length===before,"changed capability dispatched old preview");
            feed.appBound=true;
            page.connectionActions.show(target,page,0,0,false);
            Durations.defaultToken="forever";
            page.connectionActions.decide("allow");
            expect(feed.submitted.length===before,"changed Allow duration dispatched old preview");
            Durations.defaultToken="for_5_minutes";
            page.connectionActions.show(target,page,0,0,false);
            const changed=row("decision",path,"changed.example.com",ip,443); changed.action=null;
            rows.applyServerMessageJson(JSON.stringify({action:"insertConnectionRows",rows:[changed]}));
            page.connectionActions.decide("allow");
            expect(feed.submitted.length===before,"changed destination dispatched old preview");
            page.connectionActions.close();
            rows.applyServerMessageJson(JSON.stringify({action:"clearConnectionRows"}));
            rows.setGroupedMode(true);
            const known=row("group-known","/usr/bin/group","a.example.com",ip,443); known.action=null; known.process="/usr/bin/group";
            const unknown=row("group-unknown",null,"b.example.com","192.0.2.1",80); unknown.action=null; unknown.process="/usr/bin/group";
            rows.applyServerMessageJson(JSON.stringify({action:"insertConnectionRows",rows:[known,unknown]}));
        },
        function () {
            const group=page.connectionList.itemAtIndex(0);
            expect(group && group.isGroupHeader && group.groupLevel===0,"missing heterogeneous group");
            const target=page.connectionActions.snapshot(group);
            page.connectionActions.show(target,page,0,0,false);
            const preview=find(page.connectionActions.menu.contentItem,"allowPendingPreview");
            expect(preview && preview.text.indexOf("each request")>=0 && preview.text.indexOf("5 minutes")>=0
                   && preview.text.indexOf("Once")>=0 && preview.text.indexOf("host")>=0,"group preview lost per-request fallback: " + (preview ? preview.text : "missing"));
            feed.submitted=[]; page.inlineVerdicts.submittedIds=({});
            page.connectionActions.decide("allow");
            const actual=feed.submitted.slice().sort((a,b)=>a[0].localeCompare(b[0]));
            expect(JSON.stringify(actual)===JSON.stringify([["group-known","allow","this_host","for_5_minutes",true],["group-unknown","allow","this_host","this_time",false]]),"heterogeneous group payload differs: " + JSON.stringify(actual));
            // The producer substitutes IP into dstHost when DNS is absent.
            const ipOnly=row("ip-only",path,ip,ip,443); ipOnly.action=null;
            rows.applyServerMessageJson(JSON.stringify({action:"insertConnectionRows",rows:[ipOnly]}));
            rows.pendingRowIdsForProcess("");
            const payload=page.inlineVerdicts.payloadFor("ip-only","allow");
            const description=page.inlineVerdicts.scopeFor(payload);
            expect(description.indexOf("recorded destination host/IP")>=0 && description.indexOf(ip)>=0
                   && description.indexOf("exact host " + ip)<0,"IP fallback misclassified: " + description);
            expect(description.indexOf("any port")>=0 && description.indexOf("any protocol")>=0
                   && payload.scope==="this_host" && payload.duration==="for_5_minutes","IP fallback changed scope/duration");
            page.connectionActions.close();
            rows.setGroupedMode(false);
        },
        function () {
            tc.mouseClick(delegate("ip-only"),2,2);
            const explanation=find(page,"connectionExplanation");
            expect(explanation && explanation.text.indexOf("recorded destination host/IP " + ip)>=0,
                   "IP fallback explanation claims hostname provenance: " + (explanation ? explanation.text : "missing"));
            checkCopy("copyHost",ip);
            checkCopy("copyIp",ip);
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
