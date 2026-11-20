#!/usr/bin/env bash
# Run an eval scenario N times with Claude Code (your own login) and judge
# the session logs (docs/concept.md "평가 (G5c)").
#
#   scripts/eval-run.sh eval/scenarios/todo-add.json 5 [OUT_DIR]
#
# Serves examples/agent-demo on 127.0.0.1:$MB_EVAL_PORT (default 8790),
# gives each `claude -p` run its own mb-mcp (headless, --log OUT_DIR/),
# then runs `mb-mcp eval`. Answers go to OUT_DIR/answer-N.txt.
set -euo pipefail

scenario=${1:?usage: eval-run.sh SCENARIO.json [N] [OUT_DIR]}
runs=${2:-5}
root=$(cd "$(dirname "$0")/.." && pwd)
name=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$scenario")
out=${3:-$root/eval/runs/$name}
port=${MB_EVAL_PORT:-8790}
base="http://127.0.0.1:$port"
bin="$root/target/release/mb-mcp"

[ -x "$bin" ] || cargo build --release -p mb-mcp --manifest-path "$root/Cargo.toml"
mkdir -p "$out"

python3 -m http.server "$port" --bind 127.0.0.1 --directory "$root/examples/agent-demo" \
  >/dev/null 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT
sleep 1

task=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["task"].replace("{base}", sys.argv[2]))' \
  "$scenario" "$base")
config=$(python3 -c 'import json,sys; print(json.dumps({"mcpServers": {"mb": {"command": sys.argv[1], "args": ["--log", sys.argv[2] + "/"]}}}))' \
  "$bin" "$out")

for i in $(seq 1 "$runs"); do
  echo "eval-run: $name run $i/$runs" >&2
  claude -p "$task" \
    --mcp-config "$config" --strict-mcp-config \
    --allowedTools "mcp__mb" \
    --max-turns 30 \
    > "$out/answer-$i.txt" 2>&1 || echo "eval-run: run $i exited non-zero" >&2
done

"$bin" eval "$scenario" "$out" --json "$out/report.json"
