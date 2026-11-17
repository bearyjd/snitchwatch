# rpm-ostree layering (retired)

The DIY path that layered `opensnitchd` onto a stock Bazzite host and ran the
bridge as a systemd `--user` service on TCP `127.0.0.1:50051` is retired
(#35). It could not be made squat-safe, so Snitchwatch no longer documents
or ships it. The only shipped flavour is the **system bridge**.

Use one of:

- The batteries-included image: [`../../packaging/bluebuild/recipe.yml`](../../packaging/bluebuild/recipe.yml)
  with the system overlay.
- The overlay on its own: [`../../packaging/system/stage.sh`](../../packaging/system/stage.sh),
  described in [system-bridge-integration.md](system-bridge-integration.md).

This pointer promises no more than the image path does. The overlay's
daemon is the one the OpenSnitch RPM provides: `opensnitch.service` is its
unit, and `sudo systemctl enable --now opensnitch.service` starts it.
