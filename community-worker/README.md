# Community games relay

Lets anyone share a game from Rewind ("Share this game") without a GitHub account, and lets a
group of reviewers (not just the repo owner) approve them. Runs free on Cloudflare Workers.

How it works:
1. Rewind sends the game to this worker.
2. The worker opens a pull request on the repo adding `games/<ID>/`.
3. Any reviewer approves and merges it on GitHub.
4. Every Rewind install picks it up on its next start (`/index` lists the approved games, no index.json to update).

## 1. GitHub token
GitHub > Settings > Developer settings > Fine-grained tokens > Generate new token
- Repository access: **Only select repositories** > `powder-rust`
- Permissions: **Contents: Read and write**, **Pull requests: Read and write**
- Tip: make a separate GitHub account for this (e.g. `rewind-bot`, added as a collaborator) so the
  pull requests aren't opened in your name, which lets you approve them too.

## 2. Worker
dash.cloudflare.com > Workers & Pages > Create > Create Worker > name it `rewind-community` > Deploy >
Edit code > paste `worker.js` > Deploy. Then Settings > Variables and Secrets:
- `GITHUB_TOKEN` (Secret) = the token
- `REPO` (Text) = `DavidSnipeZ669/powder-rust`
- `BRANCH` (Text) = `main`

Put the worker's address (e.g. `https://rewind-community.<you>.workers.dev`) in `games/config.json`
and push. Rewind reads it from there, so no rebuild is needed.

## 3. Reviewers
- Repo > Settings > Collaborators > add each reviewer (they get Write access).
- Settings > Branches > Add branch protection rule for `main`:
  - Require a pull request before merging, Require approvals: **1** (or 2)
  - leave "Do not allow bypassing" off, so you can still push code directly
- Any reviewer can then approve and merge a submitted game. Nobody can approve their own pull request.
