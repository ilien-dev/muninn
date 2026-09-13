"""Extrae un corpus de registros tipo 'memoria de agente' desde un dump de Wikipedia.

Cada registro imita el tamaño de un recuerdo real: 60-220 palabras, con un título
que actúa como clave. No pretende ser texto de ingeniería; sirve para medir latencia
de recuperación con una distribución de términos realista (Zipf), que es lo que
determina el coste de un índice invertido.
"""
import re, json, sys, html

SRC = "/tmp/simplewiki.xml"
OUT = "/tmp/corpus.jsonl"
TARGET = int(sys.argv[1]) if len(sys.argv) > 1 else 200_000

page_re = re.compile(r"<page>(.*?)</page>", re.S)
title_re = re.compile(r"<title>(.*?)</title>", re.S)
text_re = re.compile(r'<text[^>]*>(.*?)</text>', re.S)

cleanup = [
    (re.compile(r"\{\{.*?\}\}", re.S), " "),
    (re.compile(r"\[\[(?:[^\]|]*\|)?([^\]]*)\]\]"), r"\1"),
    (re.compile(r"<ref.*?</ref>", re.S), " "),
    (re.compile(r"<[^>]+>"), " "),
    (re.compile(r"[=']{2,}"), " "),
    (re.compile(r"\s+"), " "),
]

n = 0
with open(SRC, encoding="utf-8", errors="ignore") as fh, open(OUT, "w") as out:
    buf = ""
    for chunk in iter(lambda: fh.read(1 << 22), ""):
        buf += chunk
        pos = 0
        for m in page_re.finditer(buf):
            pos = m.end()
            body = m.group(1)
            tm, xm = title_re.search(body), text_re.search(body)
            if not tm or not xm:
                continue
            title = html.unescape(tm.group(1))
            if ":" in title[:12]:            # namespaces: Category:, Template:, ...
                continue
            text = html.unescape(xm.group(1))
            for pat, rep in cleanup:
                text = pat.sub(rep, text)
            words = text.split()
            if len(words) < 60:
                continue
            for i in range(0, len(words), 160):
                seg = words[i:i + 160]
                if len(seg) < 60:
                    break
                out.write(json.dumps({"id": n, "title": title, "text": " ".join(seg)}) + "\n")
                n += 1
                if n >= TARGET:
                    print(f"{n} registros -> {OUT}")
                    raise SystemExit
        buf = buf[pos:] if pos else buf[-1 << 20:]
print(f"{n} registros -> {OUT}")
