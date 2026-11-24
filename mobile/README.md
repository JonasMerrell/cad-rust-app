# rCAD for Android

A phone app for looking at rCAD designs and designing with Claude. The phone does no modelling
and holds no Claude sign-in: everything runs on a computer at home (netdash), and the phone talks
to the same small service the rCAD Capture app uploads photos to.

- **Documents**: every rCAD document in netdash's project folders, newest first, with search.
  *New* makes a project folder with an empty document.
- **Look**: the OrcaSlicer phone app's: a dark top bar with the Features and Claude tabs, the model
  on a light scene, and a white tray you drag up from the bottom (the model stays framed above it),
  with a blue accent. The icons are the editor's own (`gen_icons.py` copies them from
  `src/icon_paths.rs`).
- **Feature tree** (Features tab): the Part Studio's features in order with the rollback bar
  (failing ones in red with the error, suppressed ones struck through), its parts and variables;
  for an assembly, its instances and mates. Tap a feature to ask Claude about it, suppress it or
  delete it (Claude does the edit on netdash).
- **Viewer**: rCAD's own interactive web page of the document (the page *File → Export web
  viewer* writes), built on netdash with `export_web` and cached until the document changes. Pick
  any Part Studio or Assembly; assemblies keep their mate sliders and animations.
- **Manufacture tabs**: the toolpath playback page (`export_cam_simulation`: stock being cut,
  tool, fixtures, time line) with a chip for the setup sheet. The tray lists the simulation's
  problems (grouped by operation and kind), the operations with tool and time, the tools and the
  operator's notes; tap a row to ask Claude about it. The app remembers the last tab per document.
- **Drawing tabs**: each sheet as vector graphics (`export_drawing` SVG): pinch to zoom, drag to
  pan, double-tap to zoom in or back out; chips switch sheets. The tray lists the sheet's views,
  dimensions, GD&T, notes and the title block.
- **Simulation tabs** (FEA): netdash solves in the background while the phone shows how far it is
  (a transient solve can take minutes), then shows rCAD's colour-map pictures: stress,
  displacement and factor of safety for a static study, each mode for a modal one, stress,
  displacement and the response over time for a transient one. The tray lists the results,
  supports and loads, material and mesh. Results are cached until the document changes.
- **Claude**: a chat under the viewer. Requests run on netdash as `rcad chat`, which is the
  editor's chat panel without the window: the same tools and instructions, signed in through
  Claude Code with your own Claude plan. Claude edits the document file, and the viewer reloads
  when the request finishes. Chats are saved in the document's `.rcad-chats/` exactly as the
  editor saves them, so a chat started on the phone continues in rCAD on the computer and the
  other way round. Requests keep running if the phone locks or loses the network; reopen the
  document to pick the reply up again. *Stop* stops a request and keeps what it changed.
- **Model**: Opus (plans and hands the work to Sonnet or Haiku, as the editor does), Sonnet or
  Haiku, or whatever the editor on netdash is set to.

## Setup on netdash

1. Build rCAD (`cargo build --release`) or unpack a release, so `rcad` is on the PATH, at
   `~/src/cad-rust-app/target/release/rcad`, or named by `RCAD_BIN`.
2. Install Claude Code and sign in once (`claude`, then `/login`). The service finds `claude` on
   its PATH; `CLAUDE_CODE_PATH` can name it instead.
3. Install or update the service (it is the rCAD Capture receiver, which now also serves this app):

   ```sh
   cp capture/receiver/rcad_capture_receiver.py capture/receiver/rcad_projects.py ~/.local/share/rcad-capture/
   cp capture/receiver/rcad-capture-receiver.service ~/.config/systemd/user/
   cp mobile/android/app/build/outputs/apk/release/app-release.apk ~/.local/share/rcad-capture/rcad.apk
   systemctl --user daemon-reload && systemctl --user restart rcad-capture-receiver
   ```

   Documents are looked for (four folders deep) in `RCAD_PROJECTS`, `~/Documents/rCAD Projects`
   and `~/rCAD Projects`; add others with `serve --projects DIR` (repeatable). `--model` sets the
   default Claude model for phone chats, and `--auth api-key` uses an API key instead of Claude Code.

## Setup on the phone

1. Open `http://192.168.0.247:8765/` and tap *Download rCAD*.
2. Open the pairing link (`python3 ~/.local/share/rcad-capture/rcad_capture_receiver.py pair-url`,
   as a QR code) and tap *Open in rCAD*. It's the same token as rCAD Capture. Or type the address
   and token in Settings.

## Build

CI (workflow `capture-apk`) builds `app-release.apk` as the artifact `rcad-apk`. Locally:
`cd mobile/android && ./gradlew assembleRelease` with `ANDROID_HOME` set. The APK is signed with
the same public sideload key as rCAD Capture, so new builds install over old ones.

## API

All on the receiver's port and token (see `capture/receiver/rcad_projects.py`):

    GET  /api/projects                     documents
    POST /api/projects {"name"}            new project
    GET  /api/doc/<id>                     tabs, saved chats, running request
    GET  /api/doc/<id>/view?studio=N|assembly=N|cam=N|drawing=N|simulation=N[&page=P]&t=<token>
    GET  /api/doc/<id>/tree?studio=N|assembly=N|cam=N|drawing=N|simulation=N   feature tree, variables, parts / instances, mates /
                                                    for other tabs: summary, pages and sections of rows
    GET  /api/doc/<id>/chats/<chat>
    POST /api/doc/<id>/chat {"text", "chat_id"?, "studio"?, "model"?, "effort"?}  → {"job"}
    GET  /api/jobs/<job>?after=N           events (long poll)
    POST /api/jobs/<job>/stop

`rcad chat --doc part.json --prompt "…"` can be used on its own too; it prints one JSON object
per line (see `src/claude_cli.rs`).

## Not done yet

- The phone reaches netdash only on the home network (port 8765 isn't open on WireGuard).
- If the document is open in the editor on netdash while Claude changes it from the phone, the
  editor doesn't reload it by itself: reopen it there.
- A git project isn't committed or pushed after a phone chat; do that from the editor.
