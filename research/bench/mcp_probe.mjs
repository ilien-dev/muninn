// Mide el coste real, extremo a extremo, de una llamada al servidor MCP oficial de
// memoria por stdio: handshake, tools/list y N llamadas a search_nodes.
import { spawn } from "node:child_process";
import fs from "node:fs";

const MEM = "/tmp/mcp_memory.jsonl";
const srv = spawn("npx", ["-y", "@modelcontextprotocol/server-memory"], {
  env: { ...process.env, MEMORY_FILE_PATH: MEM },
  stdio: ["pipe", "pipe", "inherit"],
});

let buf = "";
const pending = new Map();
srv.stdout.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i); buf = buf.slice(i + 1);
    if (!line.trim()) continue;
    let m; try { m = JSON.parse(line); } catch { continue; }
    if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
  }
});

let id = 0;
const call = (method, params) => new Promise((res) => {
  const myId = ++id;
  pending.set(myId, res);
  srv.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: myId, method, params }) + "\n");
});

const t0 = Date.now();
await call("initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "probe", version: "1" } });
srv.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
console.log(`handshake: ${Date.now() - t0} ms`);

const tl = await call("tools/list", {});
const defs = JSON.stringify(tl.result.tools);
console.log(`tools/list: ${tl.result.tools.length} tools, ${defs.length} chars JSON, ~${Math.round(defs.length / 3.6)} tokens`);

const queries = JSON.parse(fs.readFileSync("/tmp/bench_queries.json", "utf-8"));
for (const n of [1000, 5000, 20000]) {
  const rows = fs.readFileSync("/tmp/corpus.jsonl", "utf-8").split("\n").slice(0, n).filter(Boolean).map((l) => JSON.parse(l));
  fs.writeFileSync(MEM, rows.map((r) => JSON.stringify({ type: "entity", name: `e${r.id}`, entityType: "note", observations: [r.text] })).join("\n") + "\n");
  const lat = [];
  for (const q of queries.slice(0, 20)) {
    const t = process.hrtime.bigint();
    await call("tools/call", { name: "search_nodes", arguments: { query: q } });
    lat.push(Number(process.hrtime.bigint() - t) / 1e6);
  }
  lat.sort((a, b) => a - b);
  const p = (x) => lat[Math.min(lat.length - 1, Math.floor(lat.length * x))].toFixed(1);
  console.log(`search_nodes  n=${n}  p50=${p(0.5)} ms  p95=${p(0.95)} ms`);
}
srv.kill();
