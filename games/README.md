# Community games

Each folder here is one game's support pack, made with Rewind's **Add Game**:

- `game.json`: name, Steam id, authors
- `events.json`: the events to detect
- `game_postprocess.lua`: the detection rules
- `assets/`: artwork

Rewind downloads everything listed in `index.json` when it starts, so every user gets these games automatically.

## Adding a shared game (maintainers)

1. Download the `Rewind-game-<ID>.zip` from the "New game" issue and **check `game_postprocess.lua`**. It's code that runs on everyone's PC, so make sure it only does detection.
2. Unzip it here, so you get `games/<ID>/...`.
3. Double-click `update-community-index.bat` in the repo root. It rewrites `index.json`.
4. Commit and push. Everyone gets the game the next time they open Rewind.

To update a game, replace its folder and repeat steps 3 and 4. Rewind notices the change and updates it for everyone.
