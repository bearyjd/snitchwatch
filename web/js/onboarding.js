// Snitchwatch first-run wizard overlay.
//
// Calls the Tauri command `detect_daemon_state_cmd` and renders one of three
// branches when the response is not `connected`. When `connected`, removes the
// overlay entirely so the underlying app.js takes over.

(async function () {
    const overlay = document.getElementById("onboarding-overlay");
    if (!overlay) return;

    const isTauri = typeof window.__TAURI__ !== "undefined";
    if (!isTauri) {
        // Plain-browser dev mode (M2 path) — skip the wizard entirely.
        overlay.hidden = true;
        return;
    }
    const invoke = window.__TAURI__.core.invoke;

    let state = "connected";
    try {
        state = await invoke("detect_daemon_state_cmd", {
            grpcEndpoint: "127.0.0.1:50051",
        });
    } catch (err) {
        console.error("detect_daemon_state failed", err);
        state = "unreachable_retrying";
    }

    if (state === "connected") {
        overlay.hidden = true;
        return;
    }

    const card = document.createElement("div");
    card.className = "card";
    overlay.appendChild(card);
    overlay.hidden = false;

    if (state === "unit_missing") {
        card.innerHTML = `
            <h1>Welcome to Snitchwatch</h1>
            <p>We need to install the firewall daemon as a podman container.
            This is a one-time setup.</p>
            <button id="install">Install</button>
            <button class="secondary" id="cancel">Cancel</button>
        `;
        card.querySelector("#install").addEventListener("click", async () => {
            try {
                await invoke("install_daemon_stub");
            } catch (err) {
                alert(err);
            }
        });
        card.querySelector("#cancel").addEventListener("click", () => {
            overlay.hidden = true;
        });
    } else if (state === "unit_inactive") {
        card.innerHTML = `
            <h1>Daemon installed but not running</h1>
            <p>Snitchwatch needs the opensnitchd quadlet to be running to
            filter traffic.</p>
            <button id="start">Start it</button>
            <button class="secondary" id="diagnose">Diagnose</button>
        `;
        card.querySelector("#start").addEventListener("click", async () => {
            try {
                await invoke("start_daemon_unit");
                location.reload();
            } catch (err) {
                alert(err);
            }
        });
        card.querySelector("#diagnose").addEventListener("click", async () => {
            const log = await invoke("open_crash_log").catch(() => "no crash log yet");
            alert(log);
        });
    } else {
        // unreachable_retrying
        card.innerHTML = `
            <h1>Snitchwatch is reconnecting…</h1>
            <p>The daemon is installed and active but not responding yet.
            Snitchwatch is retrying with backoff.</p>
            <button class="secondary" id="dismiss">Dismiss</button>
        `;
        card.querySelector("#dismiss").addEventListener("click", () => {
            overlay.hidden = true;
        });
    }
})();
