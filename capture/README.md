# rCAD Capture

Phone app plus a small netdash service for photogrammetry capture. Photos go
straight to netdash and never into the phone's gallery: they wait in the
app's private storage only until netdash confirms each one (SHA-256 checked),
then they're deleted from the phone.

- `android/`: the Android app (Kotlin, CameraX). CI builds
  `app-release.apk` (workflow `capture-apk`, artifact `rcad-capture-apk`).
- `receiver/`: `rcad_capture_receiver.py`, Python standard library only.

## Session folders

Each capture session becomes one flat folder, which rCAD's photogrammetry
import reads:

    ~/scans/<YYYY-MM-DD_HHMM>-<name>/IMG_0001.jpg
    ~/scans/<YYYY-MM-DD_HHMM>-<name>/CAL_0002.jpg     calibration photo
    ~/scans/<YYYY-MM-DD_HHMM>-<name>/session.json

Photos are full-resolution camera JPEGs with EXIF (focal length) kept.
`session.json`:

```json
{
  "name": "intake manifold", "created": "2026-11-21T15:50:00-0700",
  "device": "Google Pixel 8", "notes": "",
  "ruler_mm": null,
  "reference": {"kind": "123-block", "label": "1-2-3 block", "dims_mm": [25.4, 50.8, 76.2]},
  "calibration_photos": ["CAL_0002.jpg"],
  "photo_count": 84, "captured": 84, "complete": true,
  "camera": {"locked": true, "focus_locked": "manual", "focal_length_mm": 6.9,
             "iso": 100, "exposure_ns": 8000000, "focus_distance_diopters": 3.1}
}
```

`complete` is false while photos are still arriving. Calibration photos
(`CAL_`) show the reference object fully in view; they are ordinary photos of
the same scene, so use them in the reconstruction too. `reference.kind` is
`123-block`, `246-block`, `ruler` (dims = mark-to-mark length, also copied
to `ruler_mm`), `box` (W, H, D) or `sphere` (diameter). The rCAD pipeline
writes its own files into `rcad-scan/` inside the folder.

## Using the app

- **Install**: open `http://<netdash>:8765/` on the phone and tap *Download
  the app* (the receiver serves `~/.local/share/rcad-capture/rcad-capture.apk`).
- **Pair**: open `http://<netdash>:8765/pair?t=<token>` on the phone (scan a
  QR code of it), then tap *Open in rCAD Capture*. `rcad_capture_receiver.py
  pair-url` prints the link. Or type the address and token in Settings.
- **Session**: press the shutter to start one and name the object.
- **Modes** (left button): *Tap / hold* (tap = one photo, hold = burst),
  *Interval* (every N seconds), *Walk-around* (a photo every N° of turn, taken
  only when the phone is steady so it isn't blurred).
- **Lock AE/AF**: tap the preview on the object to focus, then lock.
  Exposure, white balance and focus stay fixed for the session (locks by
  itself on the first photo unless turned off in Settings). Zoom is fixed at 1×.
- **Calibration photos**: put the reference (1-2-3 block, 2-4-6 block, ruler,
  other box, ball) next to the part, not touching, *before the first photo*
  and don't move it until the session is finished: reconstruction assumes a
  still scene, so a moved reference can't be found. Turn calibration on for
  the shots where it's fully in view and sharp. Long-press to change the
  reference.
- **Finish session**: marks it complete on netdash once the last photo lands.
  *Queue* lists sessions still on the phone. Uploads resume by themselves
  when the network is back, even with the app closed.

## Receiver on netdash

```sh
mkdir -p ~/.local/share/rcad-capture ~/.config/systemd/user
cp capture/receiver/rcad_capture_receiver.py ~/.local/share/rcad-capture/
cp capture/android/app/build/outputs/apk/release/app-release.apk ~/.local/share/rcad-capture/rcad-capture.apk
cp capture/receiver/rcad-capture-receiver.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now rcad-capture-receiver
python3 ~/.local/share/rcad-capture/rcad_capture_receiver.py pair-url
```

Port 8765, saves to `~/scans`, token in `~/.config/rcad-capture/token`
(created on first run). API: `GET /api/ping`, `GET /api/sessions`,
`POST /api/sessions`, `PUT /api/sessions/<id>/photos/<file>` (with
`X-Content-SHA256`), `POST /api/sessions/<id>/finish`, all with
`Authorization: Bearer <token>`.

The APK is signed with `android/app/sideload.keystore`, a deliberately public
key whose only job is to keep the signature the same between builds so
updates install over each other.
