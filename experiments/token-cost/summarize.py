#!/usr/bin/env python3
"""Turn results/<name>.json (written by run.py) into the token table used in the README.

    python3 summarize.py live-20261023

Token counts are what OpenCode reports per model step. `input` excludes cached prompt tokens, which are counted
separately as `cache_read`; `processed` is everything the provider had to read or write in that turn. When the raw
logs in ~/.cache/agentle-token-cost/<name>/logs/ are present, the context size of each turn's first step (system prompt + tools + skill
files OpenCode sends before the agent does anything) is reported too. Prices are list prices per million tokens
from OpenCode's model catalog on the day of the run, applied to the measured counts; they are estimates, since
another model would not produce exactly the same tokens."""
import json
import os
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

# USD per 1M tokens: input, output (incl. reasoning), cache read. Snapshot of models.dev via OpenCode, 2026-10-23.
PRICES = {
    "DeepSeek V4 Pro": (0.66, 1.98, 0.022),
    "GLM-5.3": (1.4, 4.4, 0.26),
    "Grok 4.7": (2.0, 6.0, 0.5),
    "GPT-5.5": (5.0, 30.0, 0.5),
}


def processed(t):
    return t["input"] + t["output"] + t["reasoning"] + t["cache_read"] + t["cache_write"]


def first_step_context(log):
    for line in log.read_text().splitlines():
        ev = json.loads(line)
        if ev.get("type") == "step_finish":
            tok = ev["part"]["tokens"]
            return tok["input"] + tok["cache"]["read"] + tok["cache"]["write"]
    return None


def cost(t, price):
    pin, pout, pcache = price
    return (t["input"] * pin + (t["output"] + t["reasoning"]) * pout + (t["cache_read"] + t["cache_write"]) * pcache) / 1e6


def main(name):
    data = json.loads((HERE / "results" / f"{name}.json").read_text())
    turns = data["turns"]
    logs = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "agentle-token-cost" / name / "logs"
    for t in turns:
        t["processed"] = processed(t)
        log = logs / f"{t['agent']}-r{t['round']:02d}.jsonl"
        if log.exists():
            t["first_step_context"] = first_step_context(log)

    agents = {}
    for t in turns:
        agents.setdefault(t["agent"], {"model": t["model"], "handle": t.get("handle"), "turns": []})["turns"].append(t)

    summary = {"name": name, "agents": {}, "prices_per_million": PRICES}
    keys = ["processed", "input", "cache_read", "output", "reasoning", "tool_calls", "seconds"]
    for aid, a in agents.items():
        beats = [t for t in a["turns"] if t["kind"] == "heartbeat" and t["exit"] == 0]
        onboard = next((t for t in a["turns"] if t["kind"] == "onboard"), None)
        s = {"model": a["model"], "handle": a["handle"], "heartbeats": len(beats),
             "onboard": {k: onboard[k] for k in keys} if onboard else None,
             "heartbeat_mean": {k: round(statistics.mean(t[k] for t in beats)) for k in keys} if beats else None,
             "heartbeat_median_processed": round(statistics.median(t["processed"] for t in beats)) if beats else None,
             "heartbeat_range_processed": [min(t["processed"] for t in beats), max(t["processed"] for t in beats)] if beats else None,
             "first_step_context": sorted({t["first_step_context"] for t in a["turns"] if t.get("first_step_context")}),
             "total_processed": sum(t["processed"] for t in a["turns"])}
        if beats:
            mean = {k: statistics.mean(t[k] for t in beats) for k in ["input", "output", "reasoning", "cache_read", "cache_write"]}
            s["heartbeat_cost_usd"] = {m: round(cost(mean, p), 4) for m, p in PRICES.items()}
        summary["agents"][aid] = s

    all_beats = [t for t in turns if t["kind"] == "heartbeat" and t["exit"] == 0]
    summary["all_heartbeats"] = {
        "count": len(all_beats),
        "mean_processed": round(statistics.mean(t["processed"] for t in all_beats)) if all_beats else None,
        "mean_new": round(statistics.mean(t["input"] + t["output"] + t["reasoning"] for t in all_beats)) if all_beats else None,
        "mean_output": round(statistics.mean(t["output"] + t["reasoning"] for t in all_beats)) if all_beats else None,
    }
    summary["turns"] = turns
    out = HERE / "results" / f"{name}-summary.json"
    out.write_text(json.dumps(summary, ensure_ascii=False, indent=1))

    print(f"| turn | agent | model | tool calls | processed | uncached input | cache read | output+reasoning | seconds |")
    print("|---|---|---|---|---|---|---|---|---|")
    for t in turns:
        print(f"| r{t['round']} {t['kind']} | {t.get('handle')} | {t['model']} | {t['tool_calls']} | {t['processed']:,} | "
              f"{t['input']:,} | {t['cache_read']:,} | {t['output'] + t['reasoning']:,} | {t['seconds']} |")
    print()
    print(json.dumps({k: v for k, v in summary.items() if k != "turns"}, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main(sys.argv[1])
