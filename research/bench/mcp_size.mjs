// ¿Cuánto texto devuelve search_nodes al contexto del modelo? No tiene LIMIT ni ranking.
import { spawn } from "node:child_process";
import fs from "node:fs";
const MEM = "/tmp/mcp_memory.jsonl";
const srv = spawn("npx", ["-y", "@modelcontextprotocol/server-memory"],
  { env: { ...process.env, MEMORY_FILE_PATH: MEM }, stdio: ["pipe", "pipe", "inherit"] });
let buf = ""; const pending = new Map();
srv.stdout.on("data", (d) => { buf += d; let i;
  while ((i = buf.indexOf("\n")) >= 0) { const l = buf.slice(0, i); buf = buf.slice(i + 1);
    if (!l.trim()) continue; let m; try { m = JSON.parse(l); } catch { continue; }
    if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } } });
let id = 0;
const call = (method, params) => new Promise((res) => { const myId = ++id;
  pending.set(myId, res); srv.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: myId, method, params }) + "\n"); });
await call("initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "p", version: "1" } });
srv.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
for (const q of ["the", "project", "was", "system design", "database"]) {
  const r = await call("tools/call", { name: "search_nodes", arguments: { query: q } });
  const txt = r.result?.content?.[0]?.text ?? "";
  console.log(`query="${q}"  respuesta=${txt.length} chars  ~${Math.round(txt.length / 3.6)} tokens`);
}
srv.kill();
