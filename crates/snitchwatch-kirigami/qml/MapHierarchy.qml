// Cached observed geography. Database coordinates remain exact approximate anchors.
import QtQuick

QtObject {
    id: hierarchy
    property var snapshot: ({
            countries: {}
        })
    property var byKey: ({})
    property var countries: []
    property var cities: []
    property var ips: []
    property var destinationByRow: ({})
    property bool available: false
    property int revision: 0

    function clear() {
        snapshot={countries:{}};byKey={};countries=[];cities=[];ips=[];destinationByRow={};available=false;revision++;
    }
    function point(coordinates) {
        return coordinates && isFinite(coordinates.latitude) && isFinite(coordinates.longitude) ? {
            x: (coordinates.longitude + 180) * 10,
            y: (90 - coordinates.latitude) * 10
        } : null;
    }
    function load(value, countryAnchors) {
        const index = {}, countryNodes = [], cityNodes = [], ipNodes = [], rows = {};
        const anchors = {};
        for (const cluster of countryAnchors)
            for (const member of cluster.members)
                anchors[member.code] = {
                    x: cluster.x,
                    y: cluster.y
                };
        for (const country of Object.values(value.countries || {})) {
            const anchor = anchors[country.code] || null;
            const cn = {
                kind: "country",
                key: country.key,
                countryKey: country.key,
                code: country.code,
                name: country.name,
                counts: country.counts,
                summary: summary(country.counts),
                position: anchor
            };
            countryNodes.push(cn);
            index[cn.key] = cn;
            for (const city of Object.values(country.cities)) {
                const position = point(city.place ? city.place.coordinates : null);
                const label = city.cityAvailable ? city.name + (city.place && city.place.subdivisions && city.place.subdivisions.length ? ", " + city.place.subdivisions.join(", ") : "") : "City unavailable";
                const node = {
                    kind: "city",
                    key: city.key,
                    countryKey: country.key,
                    code: country.code,
                    name: label,
                    counts: city.counts,
                    summary: summary(city.counts),
                    position: position || anchor,
                    approximate: !!position,
                    cityAvailable: city.cityAvailable
                };
                cityNodes.push(node);
                index[node.key] = node;
                for (const ip of Object.values(city.ips)) {
                    const ipnode = {
                        kind: "ip",
                        key: ip.key,
                        countryKey: country.key,
                        cityKey: city.key,
                        code: country.code,
                        name: ip.address,
                        canonicalIp: ip.canonicalIp,
                        counts: ip.counts,
                        summary: summary(ip.counts),
                        position: point(ip.coordinates),
                        cityName: label,
                        connections: ip.connections
                    };
                    ipNodes.push(ipnode);
                    index[ipnode.key] = ipnode;
                    for (const row of ip.connections)
                        rows[row.id] = ipnode.key;
                }
            }
        }
        snapshot = value;
        byKey = index;
        countries = countryNodes;
        cities = cityNodes;
        ips = ipNodes;
        destinationByRow = rows;
        available = true;
        revision++;
    }
    function children(kind, key) {
        if (kind === "country")
            return cities.filter(n => n.countryKey === key);
        if (kind === "city")
            return ips.filter(n => n.cityKey === key);
        if (kind === "ip")
            return byKey[key] ? [byKey[key]] : [];
        return countries;
    }
    function nodes(level, kind, key) {
        let source = level === 0 ? countries : level === 1 ? cities : ips;
        const scope = byKey[key];
        if (kind === "country")
            source = source.filter(n => n.countryKey === key);
        else if (kind === "city")
            source = source.filter(n => level === 0 ? scope && n.key === scope.countryKey : level === 1 ? n.key === key : n.cityKey === key);
        else if (kind === "ip")
            source = source.filter(n => scope && (level === 0 ? n.key === scope.countryKey : level === 1 ? n.key === scope.cityKey : n.key === key));
        return source;
    }
    function summary(counts) {
        return counts.total + " observed connections: " + counts.allowed + " allowed, " + counts.denied + " denied, " + counts.pending + " pending";
    }
    // A display fingerprint; exact selection targets remain the member IDs.
    // Length framing avoids ambiguous delimiters and bounds ListModel keys.
    function clusterKey(members) {
        let first=2166136261,second=2654435769;
        for (const member of members) {
            const framed=member.key.length+":"+member.key;
            for (let i=0;i<framed.length;i++) {
                const code=framed.charCodeAt(i);
                first=Math.imul(first^code,16777619)>>>0;
                second=Math.imul(second^code,2246822519)>>>0;
            }
        }
        return members.length+":"+first.toString(16)+":"+second.toString(16);
    }
    function clusters(nodes, scale, gap) {
        // At most ~400 cells, independent of the number of observed IPs.
        const positioned = nodes.filter(n => n.position);
        let cell = Math.max(gap / Math.max(scale, 0.001), 1);
        let bins = {};
        for (let pass = 0; pass < 12; pass++) {
            bins = {};
            for (const node of positioned) {
                const p = node.position;
                const key = Math.floor(p.x / cell) + ":" + Math.floor(p.y / cell);
                if (!bins[key])
                    bins[key] = [];
                bins[key].push(node);
            }
            if (Object.keys(bins).length <= 400)
                break;
            cell *= 2;
        }
        const out = [];
        for (const members of Object.values(bins)) {
            members.sort((a, b) => a.key.localeCompare(b.key));
            const counts = {
                total: 0,
                allowed: 0,
                denied: 0,
                pending: 0
            };
            let x = 0, y = 0;
            for (const member of members) {
                for (const field of Object.keys(counts))
                    counts[field] += member.counts[field];
                x += member.position.x;
                y += member.position.y;
                member.summary = summary(member.counts);
            }
            const dominant = counts.denied >= counts.allowed && counts.denied >= counts.pending && counts.denied > 0 ? "denied" : counts.pending >= counts.allowed && counts.pending > 0 ? "pending" : counts.allowed > 0 ? "allowed" : "none";
            const label = members.length === 1 ? members[0].name : members.length + " " + (members[0].kind === "ip" ? "observed IPs" : members[0].kind === "city" ? "cities / unavailable cities" : "countries");
            out.push({
                key: clusterKey(members),
                x: x / members.length,
                y: y / members.length,
                members: members,
                total: counts.total,
                dominant: dominant,
                mixed: [counts.allowed, counts.denied, counts.pending].filter(n => n > 0).length > 1,
                summary: label + ": " + summary(counts)
            });
        }
        return out.sort((a, b) => a.key.localeCompare(b.key));
    }
}
