# Rewind

Rewind is a community rebuild of **Powder**, the discontinued game-highlight app, rewritten in Rust so it's faster and lighter.

- **Same interface:** it keeps Powder's interface.
- **Custom game support:** you can add support for games Powder never had.
- **Your own music:** you can use your own tracks.
- **Auto-montage length:** you can set the montage length yourself.

It's shared as a **single `Rewind.exe`**. The first time it runs, it sets itself up.

> Rewind is a fan project and isn't affiliated with Powder.
> Powder's own files are downloaded from the project's Official release. Powder's staff have allowed sharing them in the Powder Discord. They aren't stored in this repository.

---

## Download

Grab one of these from the [Releases](../../releases) page and double-click it:

- **`Rewind-Full.exe`** has everything inside, so setup needs no internet. It's a big download.
- **`Rewind.exe`** is small. Setup downloads Powder's files and the AI pieces as it goes.

To build it yourself instead, follow the steps below.

---

## Running Rewind

### 1. Build it

You only need to do this once, and again after you update the code.

1. Install Rust from <https://rustup.rs>. Pick the default (MSVC) option and say **yes** to the Visual Studio C++ Build Tools.
2. Double-click **`build-app.bat`**.
3. Wait for it to finish. The first build takes a few minutes and later builds are much faster.

### 2. Open the `dist-app` folder

Open the **`dist-app`** folder. It contains a single file, **`Rewind.exe`**. That one file is the whole app, so it's also the file you share with other people.

### 3. Run `Rewind.exe` to set it up

Double-click **`Rewind.exe`**. The only choice is **where to install** (default: `%LOCALAPPDATA%\Rewind`). Click **Install Rewind** and setup does the rest:

1. **Gets Powder's recorder and AI engine.** Rewind still uses these for the parts that haven't been rewritten yet.
   - If Powder is already on this PC, setup copies from it, with no download.
   - Otherwise it downloads Powder's files from this project's [Official release](https://github.com/DavidSnipeZ669/Powder-custom-game-support/releases/tag/Official). It keeps only the parts Rewind uses, then deletes the download.
2. **Downloads the AI pieces for Add Custom Game.** That's ONNX Runtime and DirectML from Microsoft, plus the PaddleOCR text models (about 50 MB).
3. **Installs Rewind,** with Start menu and desktop shortcuts and an entry in **Apps & features**.

Then click **Open Rewind**. You don't need Powder or Steam.

**Other options** on the setup screen let you pick a different Powder folder, or get Powder through Steam instead.

---

## Using Rewind

- **Closing:** clicking the window's X sends Rewind to the **notification area** (the tray icon by the clock), just like Powder. Left-click the icon to bring the window back. Right-click it for **Start / stop session** and **Quit Rewind**.
- **Adding a game:** use **Add Game** in the sidebar, or the **"Game not listed? Add Custom Game Support"** link under the game picker. Pick a gameplay video and type the game's name, or paste its Steam link. Rewind detects the game's on-screen events and installs support for it.
- **Your own music:** put `.mp3`, `.wav`, `.ogg`, `.m4a` or `.flac` files in `%LOCALAPPDATA%\Rewind\music`.
  - They appear in every music picker.
  - Name a file `Artist - Title.mp3` to fill in the artist.
  - A picture with the same name, such as `Artist - Title.jpg`, becomes its cover.
  - Royalty-free sources include Pixabay Music, the YouTube Audio Library, Free Music Archive and Incompetech. Check each track's licence.
- **Community games:** Rewind downloads game support other people have shared, from this repo's [`games`](games) folder, every time it starts. After you add a game that works well, click **Share this game**. Rewind saves it as a zip and opens a GitHub page where you attach it. Once it's reviewed, everyone gets it.
- **Montage length:** in the auto-montage setup, drag the length slider or type an exact number of seconds in the box next to it.

---

## Uninstalling

Go to **Settings → Apps → Installed apps → Rewind → Uninstall**.

Your recordings, clips and montages aren't touched.

---

## Troubleshooting

| Problem | Fix |
|---|---|
| Rewind won't open, or the window stays blank | Install the **Microsoft Edge WebView2 Runtime**. It comes with Windows 11; on Windows 10, get it from <https://developer.microsoft.com/microsoft-edge/webview2/>. |
| Setup says a download failed | Check your internet connection and click **Try again**. Setup picks up where it stopped. |
| You want to run setup again | Run `Rewind.exe --setup`, or delete `%LOCALAPPDATA%\Rewind` and open Rewind. |
| Something else | Rewind's log file is in `%APPDATA%\powder-desktop`. Attach it when you report an issue. |

---

## Project layout

| Folder | What it is |
|---|---|
| `powder-app/` | Rewind itself: Powder's main process rewritten in Rust (native window, settings, library, montages, editor, downloads, recorder control and so on), plus first-run setup. |
| `powder-rs/` | The custom-game engine: text detection (OCR), event discovery, game installer. It's packed inside `Rewind.exe`. |
| `games/` | Community game packs, downloaded by every Rewind (see `games/README.md`). |
| `update-community-index.bat` | Rebuilds `games/index.json` after adding packs. |
| `build-app.bat` | Builds both and puts the result in `dist-app\Rewind.exe`. |
| `cleanup.bat` | Removes files the build doesn't need. |
