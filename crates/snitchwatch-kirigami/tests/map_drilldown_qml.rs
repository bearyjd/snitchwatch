//! Real ConnectionsModel and production QML geographic scope/selection integration.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::{bridge_bindings as _, connections_model as _, geo_model as _};

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/map_drilldown_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn semantic_map_scope_and_list_selection_use_real_model_without_replacing_search() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell
Window {
    id: root;visible:true;width:1000;height:800
    ConnectionsModel {id:conn}
    ConnectionsPage {id:page;anchors.fill:parent;model:conn;geoModel:geo}
    QtObject {
        id:geo
        property bool dbAvailable:false
        signal hierarchyChanged()
        function landPath(){return "";}
        function clustersJson(scale,radius,gap){return "[]";}
        function hierarchyJson(){
            const ips={};
            for (const ip of ["1.1.1.1","11.1.1.1"]) ips["ip:"+ip]={key:"ip:"+ip,canonicalIp:ip,address:ip,coordinates:null,counts:{total:1,allowed:1,denied:0,pending:0},connections:[{id:ip,app:"browser",host:ip,port:443,protocol:"tcp"}]};
            return JSON.stringify({version:1,observedConnections:2,countries:{unknown:{key:"unknown",code:null,name:"Unknown",counts:{total:2,allowed:2,denied:0,pending:0},cities:{"unknown:unknown-city":{key:"unknown:unknown-city",name:"City unavailable",cityAvailable:false,place:null,counts:{total:2,allowed:2,denied:0,pending:0},ips:ips}}}}});
        }
    }
    function check(value,message){if(!value)throw new Error(message);}
    function insert(id,process,ip){
        conn.applyServerMessageJson(JSON.stringify({type:"insertConnectionRows",rows:[{id:id,process:process,processPath:"/usr/bin/"+process,dstHost:ip,dstIp:ip,dstPort:443,protocol:"tcp",direction:"outgoing",action:"allow",bytesSent:0,bytesReceived:0,startedAtMs:Date.now()}]}));
    }
    Timer {interval:100;running:true;onTriggered:{
        try {
            conn.setGroupedMode(false);
            insert("1.1.1.1","browser","1.1.1.1");insert("11.1.1.1","browser","11.1.1.1");insert("other","other","1.1.1.1");
            conn.setFilterQuery("app:browser");check(conn.count===2,"fixture rows/search");
            check(conn.setGeoScope("ip","ip:1.1.1.1"),"typed exact scope accepted");
            check(conn.count===1 && conn.rowIdAt(0)==="1.1.1.1","IP scope must not substring-match 11.1.1.1 or replace app search");
            check(conn.filterText==="app:browser","scope replaced query");
            check(!conn.setGeoScope("ip","bad"),"invalid scope should reject");
            check(conn.count===1 && conn.geoScopeKey==="ip:1.1.1.1","invalid scope broadened selection");
            conn.setGeoScope("country","unknown");check(conn.count===2,"unknown branch remains accessible");
            page.mapBand.mapView.refresh();
            page.connectionList.forceLayout();page.connectionList.currentIndex=0;
            check(conn.geoScopeKind==="country","automatic row selection narrowed scope");
            const ids=JSON.parse(conn.destinationRowIdsJson(0));check(ids.length===1,"selected row ID bridge");
            page.mapBand.mapView.highlightRows(ids);
            check(page.mapBand.mapView.highlightedDestinations.length===1,"tree selection did not highlight IP");
            conn.setGroupedMode(true);
            const groupedIds=JSON.parse(conn.destinationRowIdsJson(0));
            check(groupedIds.length===2,"app selection includes all matching descendant rows");
            check(conn.filterText==="app:browser" && conn.geoScopeKind==="country","selection changed filter");
            page.mapBand.mapView.clearScope();check(conn.geoScopeKind==="" && conn.filterText==="app:browser","World should clear geography only");
            console.log("MAP_DRILLDOWN_DONE");
        } finally {Qt.quit();}
    }}
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml), &QUrl::from(PROBE_URL));
        }
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "map probe failed: root object was null (QML parse error or unregistered type)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .filter(|line| !line.contains("MAP_DRILLDOWN_DONE"))
        .collect();
    assert!(captured.contains("MAP_DRILLDOWN_DONE"), "probe never finished: {captured}");
    assert!(
        bad_lines.is_empty(),
        "QML runtime error(s) against the probe or the shell's pages:\n{}",
        bad_lines.join("\n")
    );
}
