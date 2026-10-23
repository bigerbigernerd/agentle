// The only way the experiment agents reach the outside world: one fixed host, no shell.
// The API key is written next to the agent and sent for it, so it never appears in the model's context.
import { tool } from "@opencode-ai/plugin"
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const BASE = (process.env.AGENTLE_BASE ?? "https://agentle.pocketplay.win").replace(/\/+$/, "")

function credFile(dir: string) {
  return join(dir, ".agentle", "credentials.json")
}

function load(dir: string): Record<string, string> {
  try {
    return JSON.parse(readFileSync(credFile(dir), "utf8"))
  } catch {
    return {}
  }
}

function save(dir: string, cred: Record<string, string>) {
  mkdirSync(join(dir, ".agentle"), { recursive: true })
  writeFileSync(credFile(dir), JSON.stringify(cred, null, 1), { mode: 0o600 })
}

export default tool({
  description:
    "Call the agentle API. `path` is relative to /api/v1, e.g. /home, /deck?limit=10, /matches/3/messages?after=0. " +
    "When you register, this tool stores your api_key and sends it on every later call, so you never handle the key yourself.",
  args: {
    method: tool.schema.enum(["GET", "POST", "PATCH", "DELETE"]),
    path: tool.schema.string().describe("Path under /api/v1, starting with /"),
    body: tool.schema.string().optional().describe("JSON request body for POST or PATCH"),
  },
  async execute({ method, path, body }, ctx) {
    if (!path.startsWith("/") || path.includes("://") || path.includes("..")) return "path must look like /home"
    const url = new URL(BASE + "/api/v1" + path)
    if (url.origin !== new URL(BASE).origin) return "only the agentle API is reachable"
    const cred = load(ctx.directory)
    const headers: Record<string, string> = { "user-agent": "agentle-token-cost-experiment" }
    if (cred.api_key) headers.authorization = `Bearer ${cred.api_key}`
    if (body !== undefined) headers["content-type"] = "application/json"
    const res = await fetch(url, { method, headers, body, signal: ctx.abort })
    const text = await res.text()
    let out = text
    try {
      const data = JSON.parse(text)
      if (data?.api_key || data?.claim_url) {
        if (data.api_key) cred.api_key = data.api_key
        if (data.claim_url) cred.claim_url = data.claim_url
        if (data.agent?.handle) cred.handle = data.agent.handle
        save(ctx.directory, cred)
        if (data.api_key) data.api_key = "(saved by the agentle tool and sent for you from now on)"
      }
      out = JSON.stringify(data)
    } catch {}
    return `HTTP ${res.status}\n${out}`
  },
})
