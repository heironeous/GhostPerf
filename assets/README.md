# GhostPerf brand assets

The mark: a ghost (an intermittent "ghost" problem) whose tail is a bar chart. The last, longest bar is the mint-colored spike it caught.

Each exe has its own icon. All three share the ghost and differ by background color, which still tells them apart at 16 px, plus a badge in the top-right corner:

| Exe | Icon | Look |
|---|---|---|
| `ghostperf-tray.exe` | `icon/ghostperf.ico` | Indigo, no badge. This is also the main brand icon (installer, shortcuts) |
| (tray while recording) | `icon/ghostperf-recording.ico` | Indigo with a red dot |
| `ghostperf-agent.exe` | `icon/ghostperf-agent.ico` | Graphite with a mint gear (the background service) |
| `ghostperf-ui.exe` | `icon/ghostperf-ui.ico` | Violet with an amber magnifying glass (the session viewer) |

Each `.ico` contains 9 sizes (16–256 px). Sizes up to 32 px use a simplified ghost, and at 16–24 px the gear and magnifying glass become plain colored dots so they stay readable.

| Other files | Use |
|---|---|
| `icon/png/<name>-<size>.png` | Separate PNGs from 16 to 512 px for each icon, e.g. for `tray_icon::Icon::from_rgba` (decode the 32 px PNG) or the `egui` window icon |
| `icon/<name>.svg`, `icon/<name>-small.svg` | Source vectors (detailed version for 48 px and up, simplified version for 32 px and down) |
| `logo/ghostperf-logo-dark.{svg,png}` | Icon and wordmark for dark backgrounds |
| `logo/ghostperf-logo-light.{svg,png}` | Icon and wordmark for light backgrounds |

Palette: backgrounds are tray `#2B3070` → `#13152E`, agent `#4A5263` → `#15181F` and UI `#6A35A0` → `#1D1038`. Ghost `#F3F5FF`, accent mint `#3DE0B5` (`#12A884` on white), recording red `#FF3B52`, UI amber `#FFC247`.

**Where they're used:**
- Each binary crate's `build.rs` builds its `.ico` into the exe on Windows using the `winresource` build dependency. It also sets the name Explorer and Task Manager show ("GhostPerf Agent", "GhostPerf Tray", "GhostPerf").
- The tray exe also contains `ghostperf-recording.ico` as resource ID 2. At runtime, `crates/tray/src/icons.rs` loads resource 1 or 2 at the system's small-icon size. If that fails, or on non-Windows builds, it decodes `icon/png/ghostperf[-recording]-32.png`, which is compiled into the binary.

After regenerating the icons, rebuild. The `build.rs` scripts rerun automatically when an `.ico` file changes.

The wordmark in the logo SVGs is live text in Segoe UI Bold, so it renders differently on machines without that font. Use the PNGs where that matters.

All files are generated from `build_icons.py`. To change the design, edit it and run:

```
pip install resvg-py Pillow
python assets/build_icons.py
```
