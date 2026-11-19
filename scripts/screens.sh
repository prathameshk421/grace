#!/usr/bin/env bash
# Make the README images: assets/hub.svg, assets/diff.svg and assets/tree.svg.
#
# It builds a fake home folder with demo projects and a demo grace database,
# runs the real grace in a private tmux server, captures each screen with its
# colors and turns the capture into an SVG with scripts/term2svg.py.
# Your own grace data, tmux server and folders are not used or changed.
#
# Usage: scripts/screens.sh [path to grace]   (default: grace on PATH)
# Needs: tmux, git, sqlite3, python3.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
grace=$(command -v "${1:-grace}")
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
H=$work/home
data=$work/data
trap 'tmux -L grace-shots kill-server 2>/dev/null; tmux -L grace-shots-hub kill-server 2>/dev/null; rm -rf "$work"' EXIT

g() { git -c user.name=demo -c user.email=demo@example.com "$@"; }

# Demo folders.
mkdir -p "$H"/{code/{payments-api,web-dashboard,ml/churn-model,ml/embeddings},work/infra,notes,dotfiles} "$data" "$work/bin"
ln -s "$grace" "$work/bin/grace"
for p in web-dashboard ml/churn-model ml/embeddings; do git -C "$H/code/$p" init -q -b main; done

# payments-api: a repo with history and open changes.
api=$H/code/payments-api
cd "$api"
git init -q -b main
mkdir -p src/routes src/db tests
cat > src/server.ts <<'EOF'
import express from "express";
import { charges } from "./routes/charges";
import { refunds } from "./routes/refunds";

const app = express();
app.use(express.json());
app.use("/charges", charges);
app.use("/refunds", refunds);

app.listen(3000, () => console.log("listening on :3000"));
EOF
cat > src/routes/charges.ts <<'EOF'
import { Router } from "express";
import { db } from "../db/client";

export const charges = Router();

charges.post("/", async (req, res) => {
  const { amount, currency } = req.body;
  const charge = await db.charges.create({ amount, currency });
  res.status(201).json(charge);
});
EOF
printf 'import { Router } from "express";\nexport const refunds = Router();\n' > src/routes/refunds.ts
printf 'export const db = {} as any;\n' > src/db/client.ts
printf '# payments-api\n\nA small payments service.\n' > README.md
printf 'node_modules\n' > .gitignore
printf '{ "name": "payments-api", "version": "0.4.0" }\n' > package.json
printf 'test("charges", () => {});\n' > tests/charges.test.ts
g add -A && g commit -qm init
g commit -q --allow-empty -m "Add refunds route"
g commit -q --allow-empty -m "Validate charge amounts"
cat > src/routes/charges.ts <<'EOF'
import { Router } from "express";
import { db } from "../db/client";
import { idempotency } from "../idempotency";

export const charges = Router();

charges.post("/", idempotency(), async (req, res) => {
  const { amount, currency, customer } = req.body;
  if (!Number.isInteger(amount) || amount <= 0) {
    return res.status(400).json({ error: "bad amount" });
  }
  const charge = await db.charges.create(req.body);
  res.status(201).json(charge);
});
EOF
cat > src/idempotency.ts <<'EOF'
import type { RequestHandler } from "express";

const seen = new Map<string, unknown>();

export const idempotency = (): RequestHandler => (req, res, next) => {
  const key = req.header("Idempotency-Key");
  if (key && seen.has(key)) return res.json(seen.get(key));
  next();
};
EOF
sed -i.bak 's/0.4.0/0.5.0/' package.json && rm package.json.bak

# Shell for the panes: a short prompt and a small PATH.
cat > "$H/.bashrc" <<EOF
export PS1='\[\e[36m\]\w\[\e[0m\] ❯ '
export PATH="$work/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
export BASH_SILENCE_DEPRECATION_WARNING=1
EOF
# The hub starts a login shell, which reads .bash_profile.
printf '. ~/.bashrc\n' > "$H/.bash_profile"

# Demo database: folders, tags and agent sessions.
run_env=(env -i HOME="$H" GRACE_DATA_DIR="$data" GRACE_TMUX_SOCKET=grace-shots-hub
  TERM=xterm-256color COLORTERM=truecolor SHELL=/bin/bash LANG=en_US.UTF-8
  PATH="$work/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
"${run_env[@]}" grace list >/dev/null
now=$(date +%s)
sqlite3 "$data/grace.db" <<EOF
DELETE FROM dirs; DELETE FROM tags; DELETE FROM sessions;
INSERT INTO dirs VALUES
 ('$H/code/payments-api', 42, $now-60, 1),
 ('$H/code/web-dashboard', 31, $now-300, 0),
 ('$H/code/ml/churn-model', 18, $now-900, 0),
 ('$H/code/ml/embeddings', 9, $now-7200, 0),
 ('$H/work/infra', 14, $now-86400, 0),
 ('$H/notes', 6, $now-259200, 0),
 ('$H/dotfiles', 3, $now-1728000, 0);
INSERT INTO tags VALUES
 ('$H/code/payments-api','claude',30,$now-60), ('$H/code/payments-api','cursor',8,$now-4000),
 ('$H/code/web-dashboard','codex',12,$now-300), ('$H/code/web-dashboard','vscode',20,$now-5000),
 ('$H/code/ml/churn-model','claude',10,$now-900), ('$H/code/ml/churn-model','gemini',3,$now-90000),
 ('$H/code/ml/embeddings','antigravity',5,$now-7200),
 ('$H/work/infra','opencode',4,$now-86400), ('$H/work/infra','zed',6,$now-86400),
 ('$H/notes','vscode',6,$now-259200),
 ('$H/dotfiles','neovim',3,$now-1728000);
INSERT INTO sessions(path, tool, status, shell_pid, started_at, updated_at) VALUES
 ('$H/code/payments-api','claude','waiting',NULL,$now-1500,$now-45),
 ('$H/code/web-dashboard','codex','working',NULL,$now-900,$now-10),
 ('$H/code/ml/churn-model','claude','ready',NULL,$now-4000,$now-600),
 ('$H/code/ml/embeddings','antigravity','done',NULL,$now-5000,$now-1200);
EOF

# shot <name> <cols> <rows> <command> <wait> <label> [keys...]
shot() {
  local name=$1 cols=$2 rows=$3 cmd=$4 wait=$5 label=$6
  shift 6
  local t=(tmux -L grace-shots -f /dev/null)
  "${t[@]}" kill-server 2>/dev/null || true
  "${t[@]}" new-session -d -s s -x "$cols" -y "$rows" -c "$api" \
    "$(printf '%q ' "${run_env[@]}") /bin/bash --rcfile $H/.bashrc -i"
  "${t[@]}" set -g status off
  sleep 1
  "${t[@]}" send-keys -t s "clear; $cmd" Enter
  sleep "$wait"
  for k in "$@"; do "${t[@]}" send-keys -t s "$k"; sleep 0.6; done
  "${t[@]}" capture-pane -t s -e -p > "$work/$name.txt"
  "${t[@]}" kill-server
  tmux -L grace-shots-hub kill-server 2>/dev/null || true
  python3 "$repo/scripts/term2svg.py" "$work/$name.txt" "$repo/assets/$name.svg" \
    "~/code/payments-api ❯ $cmd" "$label"
  echo "wrote assets/$name.svg"
}

label_hub="The grace hub: a sidebar of agents and folders, a shell, a live diff and a file tree in one window"
label_diff="grace diff: changed files on the left, the colored diff on the right"
label_tree="grace tree: the files of a folder with git changes marked"
shot hub 140 24 "grace hub" 4 "$label_hub" 'git log --oneline -3' Enter 'git status -s' Enter
shot diff 112 24 "grace diff" 2 "$label_diff" j
shot tree 60 16 "grace tree" 2 "$label_tree" Enter j j Enter
