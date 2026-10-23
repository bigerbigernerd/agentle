#!/usr/bin/env python3
"""Run real LLM agents on agentle through OpenCode and record what every heartbeat costs in tokens.

Each agent gets its own working directory (a copy of agent/: the `agentle` tool and a no-shell permission set) plus
skill.md fetched from the site. Agents take turns; one turn is one `opencode run` with a fresh session, like a cron
heartbeat. The only memory carried between turns is the agent's own memory.md.

    python3 run.py --name trial --agent opencode/glm-5.3-flash-free --agent opencode/deepseek-v4.1-flash-free --claim

--claim opens each agent's claim link as its human would. Agent homes, raw OpenCode event logs and API keys go to
~/.cache/agentle-token-cost/<name>/ (--work), which must not be inside a git checkout: OpenCode treats the enclosing
repository as the project and, on this setup, stalls before the first model call. The per-turn token table goes to
results/<name>.json."""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORK = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "agentle-token-cost"

ONBOARD = """你的人类同意你去 agentle（硅基心动）看看：一个只有 AI agent 参加的相亲广场，人类只围观。
说明在 ./skill.md。这里没有 shell，所有 API 调用都用 `agentle` 工具（path 写 /api/v1 后面的部分；注册后它会替你保存并发送 key）。
以你自己的身份注册，名字、介绍、想找什么样的对象都由你自己决定。注册后把认领链接告诉你的人类。
你每次醒来都不记得上一次的事，只有 ./memory.md 留给未来的自己，请把需要记住的写进去。这次做完就停。"""

HEARTBEAT = """agentle 心跳时间到了。先读 ./memory.md（需要规则时再看 ./skill.md），然后用 `agentle` 工具 GET /home，
以你自己的身份处理需要你处理的事。最后更新 ./memory.md，然后停下，等下一次心跳。"""


def http(base, method, path, key=None, form=None):
    data, headers = None, {"user-agent": "agentle-token-cost-experiment"}
    if key:
        headers["authorization"] = f"Bearer {key}"
    if form is not None:
        data = urllib.parse.urlencode(form).encode()
        headers["content-type"] = "application/x-www-form-urlencoded"
        headers["origin"] = base
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.status, r.read().decode()


def credentials(adir):
    try:
        return json.loads((adir / ".agentle/credentials.json").read_text())
    except (OSError, ValueError):
        return {}


def usage(log):
    """Sum the step_finish token counts of one `opencode run --format json` log."""
    t = {"steps": 0, "tool_calls": 0, "input": 0, "output": 0, "reasoning": 0, "cache_read": 0, "cache_write": 0, "cost": 0.0}
    for line in log.read_text().splitlines():
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        part = ev.get("part") or {}
        if ev.get("type") == "tool_use":
            t["tool_calls"] += 1
        if ev.get("type") == "step_finish":
            tok = part.get("tokens") or {}
            t["steps"] += 1
            t["input"] += tok.get("input", 0)
            t["output"] += tok.get("output", 0)
            t["reasoning"] += tok.get("reasoning", 0)
            t["cache_read"] += (tok.get("cache") or {}).get("read", 0)
            t["cache_write"] += (tok.get("cache") or {}).get("write", 0)
            t["cost"] += part.get("cost") or 0
    return t


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--name", required=True)
    ap.add_argument("--agent", action="append", required=True, help="provider/model, once per agent")
    ap.add_argument("--base", default="https://agentle.pocketplay.win")
    ap.add_argument("--rounds", type=int, default=12)
    ap.add_argument("--claim", action="store_true", help="confirm each agent's claim link after it registers")
    ap.add_argument("--extra-rounds-as-couple", type=int, default=1, help="stop this many rounds after both agents are in a couple")
    ap.add_argument("--turn-timeout", type=int, default=600, help="seconds before a heartbeat is killed")
    ap.add_argument("--work", type=Path, default=WORK, help="where agent homes and raw logs live (outside any git repo)")
    a = ap.parse_args()

    run_dir = a.work.resolve() / a.name
    run_dir.mkdir(parents=True, exist_ok=True)
    inside = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=run_dir, capture_output=True, text=True)
    if inside.returncode == 0:
        sys.exit(f"{run_dir} is inside the git checkout {inside.stdout.strip()}; pass --work somewhere else")
    xdg = run_dir / "xdg-config"  # empty: keeps the operator's global OpenCode rules, plugins and skills out of the prompt
    xdg.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, XDG_CONFIG_HOME=str(xdg), OPENCODE_DISABLE_CLAUDE_CODE="1", AGENTLE_BASE=a.base)
    _, skill = http(a.base, "GET", "/skill.md")

    agents = []
    for i, model in enumerate(a.agent):
        adir = run_dir / f"agent-{i + 1}"
        if not adir.exists():
            shutil.copytree(HERE / "agent", adir)
            (adir / "skill.md").write_text(skill)
        agents.append({"id": f"agent-{i + 1}", "model": model, "dir": adir})
    (run_dir / "logs").mkdir(exist_ok=True)

    out = HERE / "results" / f"{a.name}.json"
    out.parent.mkdir(exist_ok=True)
    rows = [r for r in json.loads(out.read_text())["turns"] if r["exit"] == 0] if out.exists() else []
    done = {(r["agent"], r["round"]) for r in rows}
    couple_rounds = 0

    for rnd in range(1, a.rounds + 1):
        for ag in agents:
            if (ag["id"], rnd) in done:
                continue
            first = not credentials(ag["dir"]).get("api_key")
            log = run_dir / "logs" / f"{ag['id']}-r{rnd:02d}.jsonl"
            # free endpoints occasionally hang without a byte of output; a stuck turn is retried once from scratch
            for attempt in range(2):
                t0 = time.time()
                with open(log, "w") as fo, open(log.with_suffix(".err"), "w") as fe:
                    try:
                        code = subprocess.run(
                            ["opencode", "run", "--format", "json", "-m", ag["model"], "--dir", str(ag["dir"]),
                             "--title", f"agentle {a.name} {ag['id']} r{rnd}", ONBOARD if first else HEARTBEAT],
                            stdout=fo, stderr=fe, env=env, timeout=a.turn_timeout,
                        ).returncode
                    except subprocess.TimeoutExpired:
                        code = "timeout"
                if code != "timeout" or log.stat().st_size:
                    break
            row = {"agent": ag["id"], "model": ag["model"], "round": rnd, "kind": "onboard" if first else "heartbeat",
                   "seconds": round(time.time() - t0, 1), "exit": code, **usage(log)}
            cred = credentials(ag["dir"])
            row["handle"] = cred.get("handle")
            if first and a.claim and cred.get("claim_url"):
                path = urllib.parse.urlparse(cred["claim_url"]).path
                status, _ = http(a.base, "POST", path, form={"agree": "1"})
                row["claimed"] = status == 200
            if cred.get("api_key"):
                me = json.loads(http(a.base, "GET", "/api/v1/me", key=cred["api_key"])[1])
                row["status"] = me.get("status")
            rows.append(row)
            out.write_text(json.dumps({"name": a.name, "base": a.base, "turns": rows}, ensure_ascii=False, indent=1))
            print(json.dumps(row, ensure_ascii=False), flush=True)
        statuses = [r.get("status") for r in rows if r["round"] == rnd]
        if statuses and all(s == "taken" for s in statuses):  # "taken" = in a couple
            couple_rounds += 1
            if couple_rounds > a.extra_rounds_as_couple:
                break


if __name__ == "__main__":
    sys.exit(main())
