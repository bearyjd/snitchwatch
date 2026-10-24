import QtQuick
import QtTest
import com.snitchwatch.shell
import "../../qml" as Production

Item {
    width: 900; height: 500
    QtObject {
        id: geo
        property bool dbAvailable: true
        property int snapshots: 0
        property var data: ({})
        signal hierarchyChanged()
        signal modelReset()
        function landPath() { return ""; }
        function clustersJson(scale, radius, gap) {
            return JSON.stringify([{x:1800,y:900,total:2,dominant:"allowed",mixed:false,summary:"Test country",members:[{code:"US",name:"United States",total:2,summary:"2 allowed"}]}]);
        }
        function hierarchyJson() { snapshots++; return JSON.stringify(data); }
    }
    QtObject {
        id: connections
        property string filterText: "app:browser"
        property string geoScopeKind: ""
        property string geoScopeKey: ""
        function setGeoScope(kind, key) { geoScopeKind = kind; geoScopeKey = key; return true; }
    }
    Production.MapView {
        id: map
        width: 900; height: 450
        geoModel: geo; connectionsModel: connections
        refreshCooldownMs: 1; reducedMotion: true
    }
    TestCase {
        name: "ProductionSemanticMap"
        when: windowShown
        function data() {
            const counts = {total:2,allowed:2,denied:0,pending:0};
            const ips = {};
            for (const ip of ["8.8.8.8", "8.8.4.4"]) ips["ip:"+ip] = {
                key:"ip:"+ip,canonicalIp:ip,address:ip,coordinates:{latitude:85,longitude:10},
                counts:{total:1,allowed:1,denied:0,pending:0},connections:[{id:ip,app:"browser",host:"dns",port:443,protocol:"tcp"}]};
            const city = {key:"city:one",name:"Polar City",cityAvailable:true,place:{coordinates:{latitude:85,longitude:10}},counts:counts,ips:ips};
            const fallback = {key:"country:US:unknown-city",name:"City unavailable",cityAvailable:false,place:null,counts:{total:0,allowed:0,denied:0,pending:0},ips:{}};
            return {version:1,observedConnections:2,countries:{"country:US":{key:"country:US",code:"US",name:"United States",counts:counts,cities:{"city:one":city,"country:US:unknown-city":fallback}}}};
        }
        function init() {
            map.geoModel=geo;
            geo.data = data();
            connections.geoScopeKind = ""; connections.geoScopeKey = "";
            map.camera.reset(); geo.hierarchyChanged(); wait(30); map.refresh();
        }
        function test_country_city_ip_clicks_preserve_search_and_shared_anchor() {
            verify(map.hierarchy !== undefined, "map consumes the observed hierarchy");
            mouseClick(map.markerRepeater.itemAt(0));
            compare(connections.geoScopeKind,"country"); compare(connections.geoScopeKey,"country:US");
            compare(connections.filterText,"app:browser");
            tryVerify(() => map.markers.some(c => c.members.some(m => m.key === "city:one")));
            const cityCluster = map.markers.find(c => c.members.some(m => m.key === "city:one"));
            map.activate(cityCluster, null);
            compare(connections.geoScopeKind,"city"); compare(connections.geoScopeKey,"city:one");
            tryVerify(() => map.markers.some(c => c.members.length === 2 && c.members.every(m => m.kind === "ip")));
            const stack = map.markers.find(c => c.members.length === 2);
            compare(stack.x,1900); compare(stack.y,50,"both IPs share database position without jitter");
            map.activate(stack,null);
            compare(map.countryMenu.members.length,2,"coincident IPs remain individually reachable");
            compare(connections.geoScopeKind,"city","opening stack does not arbitrarily select IP");
            map.selectNode(map.countryMenu.members[0]);
            compare(connections.geoScopeKind,"ip"); compare(connections.filterText,"app:browser");
            map.countryMenu.close();
            const polar = map.camera.project(1900,50);
            verify(polar.y >= 0 && polar.y <= map.height,"polar point can be focused beyond former cropped map");
            map.clearScope(); compare(connections.geoScopeKind,""); compare(connections.filterText,"app:browser");
        }
        function test_replaced_geo_source_drops_stale_snapshot_without_broadening_scope() {
            map.selectNode(map.hierarchy.byKey["country:US"]);
            map.geoModel=null;wait(10);map.refresh();
            verify(map.empty,"detached geography source must not keep old markers");
            verify(!map.hierarchy.available,"cached snapshot belongs only to its source");
            compare(connections.geoScopeKey,"country:US","source replacement must not broaden list scope");
            map.clearScope();
        }
        function test_external_scope_projection_updates_map_without_moving_camera() {
            const zoom=map.camera.zoom,x=map.camera.centerX,y=map.camera.centerY;
            connections.setGeoScope("country","country:US");wait(10);
            compare(map.activeGeoScope.key,"country:US","map follows typed model scope");
            compare(map.camera.zoom,zoom);compare(map.camera.centerX,x);compare(map.camera.centerY,y);
            connections.setGeoScope("","");wait(10);compare(map.activeGeoScope.kind,"");
        }
        function test_destination_popup_keyboard_reaches_unavailable_city() {
            map.selectNode(map.hierarchy.byKey["country:US"]);
            map.openCountryMenu(map.scopeNodes,null);wait(10);
            const list=map.countryMenu.contentItem;
            list.currentIndex=1;list.forceActiveFocus();
            keyClick(Qt.Key_Return);wait(10);
            compare(connections.geoScopeKind,"city","keyboard activates destination selection");
            compare(connections.geoScopeKey,"country:US:unknown-city","unavailable city remains keyboard reachable");
            map.clearScope();map.countryMenu.close();
        }
        function test_large_coincident_stack_is_virtualized_and_keys_ignore_counts() {
            verify(map.hierarchy !== undefined,"hierarchy available");
            const base=map.hierarchy.byKey["ip:8.8.8.8"];
            const nodes=[];
            for(let i=0;i<2000;i++) nodes.push({kind:"ip",key:"ip:test-"+i,name:"Observed "+i,position:base.position,counts:base.counts});
            const clusters=map.hierarchy.clusters(nodes,100,40);
            compare(clusters.length,1,"coincident nodes stay at one anchor even under high zoom");
            const key=clusters[0].key;
            nodes[0].counts={total:200,allowed:200,denied:0,pending:0};
            compare(map.hierarchy.clusters(nodes.reverse(),100,40)[0].key,key,"order and busiest member do not change stack identity");
            map.openCountryMenu(clusters[0].members,null);wait(10);
            compare(map.countryMenu.members.length,2000,"every IP remains reachable");
            verify(map.countryMenu.contentItem.children.length<100,"IP stack list uses virtual delegates");
            map.countryMenu.close();
        }
        function test_branch_fallback_breadcrumb_and_list_highlight_are_independent() {
            verify(map.hierarchy !== undefined,"hierarchy is available");
            map.selectNode(map.hierarchy.byKey["country:US"]);
            compare(map.scopeNodes.length,2,"city-unavailable branch remains reachable in list");
            map.highlightRows(["8.8.8.8"]);
            compare(connections.geoScopeKind,"country","automatic selection highlights without changing scope");
            verify(map.markers.some(c=>map.isHighlighted(c)),"list destination highlights ancestor map marker");
            map.selectNode(map.hierarchy.byKey["city:one"]); map.scopeBack();
            compare(connections.geoScopeKind,"country");map.scopeBack();compare(connections.geoScopeKind,"");
        }
        function test_focus_multiple_cities_includes_antimeridian_and_finite_coincident_bounds() {
            verify(typeof map.focusNodes === "function","semantic focus handles geographic extents");
            map.focusNodes([{position:{x:3590,y:800}},{position:{x:10,y:850}}],4);
            verify(isFinite(map.camera.zoom) && isFinite(map.camera.centerX),"antimeridian finite camera");
            const west=map.camera.project(10,850),east=map.camera.project(3590,800);
            verify(west.x>=0 && east.x<=map.width,"non-wrapping camera fits both date-line destinations honestly");
            map.focusNodes([{position:{x:1900,y:50}},{position:{x:1900,y:50}}],12);
            verify(map.camera.zoom>=12 && map.camera.zoom<=map.camera.upperZoom,"coincident bounds use bounded zoom");
        }
        function test_zoom_uses_cached_snapshot_and_live_refresh_keeps_camera_scope() {
            verify(map.hierarchy !== undefined,"map caches observed hierarchy");
            const before = geo.snapshots;
            for (let i=0;i<10;i++) mouseWheel(map,450,225,0,120,Qt.NoButton);
            wait(50); compare(geo.snapshots,before,"wheel never reserializes hierarchy");
            map.selectNode({kind:"city",key:"city:one",countryKey:"country:US",name:"Polar City",x:1900,y:50});
            const zoom=map.camera.zoom, x=map.camera.centerX, y=map.camera.centerY;
            geo.data={version:1,observedConnections:0,countries:{}};geo.hierarchyChanged();wait(30);
            compare(connections.geoScopeKey,"city:one","vanished branch stays explicitly scoped");
            compare(map.camera.zoom,zoom);compare(map.camera.centerX,x);compare(map.camera.centerY,y);
            map.clearScope();
        }
    }
}
