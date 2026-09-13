// Réplica fiel de loadGraph() + searchNodes() del servidor MCP oficial de memoria
// (modelcontextprotocol/servers/src/memory/index.ts): lee el fichero entero,
// JSON.parse por línea, filtro includes() en minúsculas. Sin índice.
import fs from "node:fs/promises";

const FILE = "/tmp/bench_scan.jsonl";
const queries = JSON.parse(await fs.readFile("/tmp/bench_queries.json", "utf-8"));

async function loadGraph() {
  const data = await fs.readFile(FILE, "utf-8");
  const lines = data.split("\n").filter((l) => l.trim() !== "");
  return lines.map((l) => JSON.parse(l));
}

async function searchNodes(query) {
  const g = await loadGraph();
  const q = query.toLowerCase();
  return g.filter(
    (e) => e.title.toLowerCase().includes(q) || e.text.toLowerCase().includes(q),
  );
}

const lat = [];
for (const q of queries) {
  const t = process.hrtime.bigint();
  await searchNodes(q);
  lat.push(Number(process.hrtime.bigint() - t) / 1e6);
}
lat.sort((a, b) => a - b);
const p = (x) => lat[Math.min(lat.length - 1, Math.floor(lat.length * x))].toFixed(3);
console.log(`n_queries=${lat.length} p50=${p(0.5)} p95=${p(0.95)} p99=${p(0.99)}`);
