import { useEffect, useMemo, useRef, useState } from "react";
import type { SheetData } from "./sheet.worker";
import { Icon } from "./icons";

// Heavy parsers load on first use so the file browser stays small.
const loadHljs = () => import("highlight.js/lib/common").then((m) => m.default);
const loadPurify = () => import("dompurify").then((m) => m.default);

const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const MAX_HIGHLIGHT = 300_000;

/** Plain textarea over a highlighted copy of the same text, with line numbers. */
export function CodeEditor(props: { value: string; onChange: (v: string) => void; lang?: string | undefined; readOnly?: boolean }) {
  const { value, lang } = props;
  const [html, setHtml] = useState(() => escapeHtml(value));
  const pre = useRef<HTMLPreElement>(null);
  const gutter = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    if (!lang || value.length > MAX_HIGHLIGHT) {
      setHtml(escapeHtml(value));
      return;
    }
    loadHljs().then((hljs) => {
      if (!live) return;
      const out = hljs.getLanguage(lang) ? hljs.highlight(value, { language: lang, ignoreIllegals: true }).value : escapeHtml(value);
      setHtml(out);
    });
    return () => {
      live = false;
    };
  }, [value, lang]);

  const lines = useMemo(() => value.split("\n").length, [value]);

  const sync = (e: React.UIEvent<HTMLTextAreaElement>) => {
    const { scrollTop, scrollLeft } = e.currentTarget;
    if (pre.current) {
      pre.current.scrollTop = scrollTop;
      pre.current.scrollLeft = scrollLeft;
    }
    if (gutter.current) gutter.current.scrollTop = scrollTop;
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Tab indents inside the editor; Escape then Tab moves focus on.
    if (e.key !== "Tab" || e.shiftKey || props.readOnly) return;
    e.preventDefault();
    const el = e.currentTarget;
    const { selectionStart: a, selectionEnd: b } = el;
    props.onChange(value.slice(0, a) + "  " + value.slice(b));
    requestAnimationFrame(() => el.setSelectionRange(a + 2, a + 2));
  };

  return (
    <div className={props.readOnly ? "code readonly" : "code"}>
      <div className="gutter" ref={gutter} aria-hidden="true">
        {Array.from({ length: lines }, (_, i) => (
          <div key={i}>{i + 1}</div>
        ))}
      </div>
      <div className="code-area">
        <pre ref={pre} className="hljs code-hl" aria-hidden="true">
          <code dangerouslySetInnerHTML={{ __html: html + "\n" }} />
        </pre>
        <textarea
          className="code-input"
          value={value}
          readOnly={props.readOnly}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          wrap="off"
          aria-label="File contents"
          onChange={(e) => props.onChange(e.target.value)}
          onScroll={sync}
          onKeyDown={onKeyDown}
        />
      </div>
    </div>
  );
}

export function MarkdownView({ source }: { source: string }) {
  const [html, setHtml] = useState("");
  const root = useRef<HTMLElement>(null);
  useEffect(() => {
    let live = true;
    Promise.all([import("marked"), loadPurify()]).then(([{ marked }, purify]) => {
      const raw = marked.parse(source, { async: false, gfm: true }) as string;
      if (live) setHtml(purify.sanitize(raw));
    });
    return () => {
      live = false;
    };
  }, [source]);
  // Highlight fenced code once the rendered HTML is in the page.
  useEffect(() => {
    const blocks = root.current?.querySelectorAll<HTMLElement>("pre code");
    if (!blocks?.length) return;
    loadHljs().then((hljs) => blocks.forEach((el) => hljs.highlightElement(el)));
  }, [html]);
  return <article ref={root} className="page markdown" dangerouslySetInnerHTML={{ __html: html }} />;
}

/** HTML renders in a sandbox with no scripts and an opaque origin. */
export function HtmlView({ source }: { source: string }) {
  return <iframe className="frame" title="HTML preview" sandbox="" srcDoc={source} />;
}

export function DocxView({ blob }: { blob: Blob }) {
  const [html, setHtml] = useState<string | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    Promise.all([import("mammoth"), loadPurify(), blob.arrayBuffer()])
      .then(async ([mammoth, purify, arrayBuffer]) => {
        const out = await mammoth.convertToHtml({ arrayBuffer });
        if (live) setHtml(purify.sanitize(out.value));
      })
      .catch((e) => live && setError(String(e?.message ?? e)));
    return () => {
      live = false;
    };
  }, [blob]);
  if (error) return <Notice title="This document could not be previewed" body={error} />;
  if (html === null) return <Loading />;
  if (!html.trim()) return <Notice title="This document is empty" body="" />;
  return (
    <div className="page-scroll">
      <article className="page docx" dangerouslySetInnerHTML={{ __html: html }} />
    </div>
  );
}

export function SheetView({ blob, text, delimiter }: { blob?: Blob; text?: string; delimiter?: "," | "\t" }) {
  const [sheets, setSheets] = useState<SheetData[] | null>(null);
  const [active, setActive] = useState(0);
  const [error, setError] = useState("");

  useEffect(() => {
    let done = false;
    const worker = new Worker(new URL("./sheet.worker.ts", import.meta.url), { type: "module" });
    const timer = setTimeout(() => {
      if (done) return;
      worker.terminate();
      setError("Parsing took too long, so it was stopped. Download the file to open it.");
    }, 20_000);
    worker.onmessage = (e: MessageEvent<{ ok: boolean; sheets?: SheetData[]; error?: string }>) => {
      done = true;
      clearTimeout(timer);
      worker.terminate();
      if (e.data.ok) setSheets(e.data.sheets ?? []);
      else setError(e.data.error ?? "unknown error");
    };
    (async () => {
      if (text !== undefined) worker.postMessage({ data: text, csv: delimiter });
      else if (blob) {
        const buf = await blob.arrayBuffer();
        worker.postMessage({ data: buf }, [buf]);
      }
    })();
    return () => {
      done = true;
      clearTimeout(timer);
      worker.terminate();
    };
  }, [blob, text, delimiter]);

  if (error) return <Notice title="This spreadsheet could not be previewed" body={error} />;
  if (!sheets) return <Loading />;
  const sheet = sheets[Math.min(active, sheets.length - 1)];
  if (!sheet) return <Notice title="This spreadsheet has no sheets" body="" />;
  const cols = Math.max(1, ...sheet.rows.map((r) => r.length));
  return (
    <div className="sheet">
      <div className="sheet-scroll">
        <table>
          <thead>
            <tr>
              <th className="corner" />
              {Array.from({ length: cols }, (_, i) => (
                <th key={i}>{colName(i)}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {sheet.rows.map((r, i) => (
              <tr key={i}>
                <th>{i + 1}</th>
                {Array.from({ length: cols }, (_, j) => (
                  <td key={j}>{r[j] ?? ""}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {(sheet.totalRows > sheet.rows.length || sheet.totalCols > cols) && (
        <div className="sheet-note muted small">
          Showing {sheet.rows.length} of {sheet.totalRows} rows and {cols} of {sheet.totalCols} columns. Download the file for the rest.
        </div>
      )}
      {sheets.length > 1 && (
        <div className="sheet-tabs" role="tablist">
          {sheets.map((s, i) => (
            <button key={s.name} role="tab" aria-selected={i === active} className={i === active ? "on" : ""} onClick={() => setActive(i)}>
              {s.name}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

const colName = (i: number): string => (i < 26 ? String.fromCharCode(65 + i) : colName(Math.floor(i / 26) - 1) + colName(i % 26));

function useObjectUrl(blob: Blob, type?: string) {
  const url = useMemo(() => URL.createObjectURL(type ? new Blob([blob], { type }) : blob), [blob, type]);
  useEffect(() => () => URL.revokeObjectURL(url), [url]);
  return url;
}

/** Renders every page with pdf.js onto canvases. Scripts inside the PDF never run, and eval is off. */
export function PdfView({ blob }: { blob: Blob }) {
  const host = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<{ pages: number } | { error: string } | null>(null);

  useEffect(() => {
    let live = true;
    let destroy = () => {};
    (async () => {
      const [pdfjs, { default: workerUrl }] = await Promise.all([
        import("pdfjs-dist"),
        import("pdfjs-dist/build/pdf.worker.min.mjs?url"),
      ]);
      pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;
      const task = pdfjs.getDocument({ data: new Uint8Array(await blob.arrayBuffer()), isEvalSupported: false });
      destroy = () => void task.destroy();
      const doc = await task.promise;
      if (!live || !host.current) return;
      host.current.replaceChildren();
      setState({ pages: doc.numPages });
      const width = Math.min(host.current.clientWidth - 48, 900);
      for (let n = 1; n <= doc.numPages && live; n++) {
        const page = await doc.getPage(n);
        const base = page.getViewport({ scale: 1 });
        const scale = width / base.width;
        const viewport = page.getViewport({ scale: scale * window.devicePixelRatio });
        const canvas = document.createElement("canvas");
        const context = canvas.getContext("2d");
        if (!context) throw new Error("This browser did not provide a 2D canvas for the PDF page.");
        canvas.width = viewport.width;
        canvas.height = viewport.height;
        canvas.style.width = `${base.width * scale}px`;
        canvas.setAttribute("aria-label", `Page ${n}`);
        host.current?.appendChild(canvas);
        await page.render({ canvasContext: context, viewport }).promise;
      }
    })().catch((e) => live && setState({ error: String(e?.message ?? e) }));
    return () => {
      live = false;
      destroy();
    };
  }, [blob]);

  if (state && "error" in state) return <Notice title="This PDF could not be previewed" body={state.error} />;
  return (
    <div className="pdf">
      {!state && <Loading />}
      <div className="pdf-pages" ref={host} />
    </div>
  );
}

export function ImageView({ blob, name, svg }: { blob: Blob; name: string; svg?: boolean }) {
  // SVG shown through <img> never runs its scripts.
  const url = useObjectUrl(blob, svg ? "image/svg+xml" : undefined);
  return (
    <div className="media checker">
      <img src={url} alt={name} />
    </div>
  );
}

export function MediaView({ blob, kind }: { blob: Blob; kind: "audio" | "video" }) {
  const url = useObjectUrl(blob);
  return <div className="media">{kind === "audio" ? <audio src={url} controls /> : <video src={url} controls />}</div>;
}

export function BinaryCard(props: { name: string; label: string; size: string; onDownload: () => void }) {
  return (
    <div className="notice">
      <Icon name="file" size={40} />
      <h3>{props.name}</h3>
      <p className="muted">
        {props.label} · {props.size}. There is no preview for this type yet.
      </p>
      <button className="btn primary" onClick={props.onDownload}>
        <Icon name="download" size={18} /> Download
      </button>
    </div>
  );
}

function Notice({ title, body }: { title: string; body: string }) {
  return (
    <div className="notice">
      <h3>{title}</h3>
      {body && <p className="muted">{body}</p>}
    </div>
  );
}

export function Loading() {
  return <div className="notice muted">Loading preview…</div>;
}

/** A unified diff with added and removed lines coloured, and old/new line numbers. */
export function DiffView({ diff }: { diff: string }) {
  const rows = useMemo(() => {
    const out: { kind: "add" | "del" | "ctx" | "hunk"; text: string; a?: number; b?: number }[] = [];
    let a = 0;
    let b = 0;
    for (const line of diff.split("\n")) {
      if (line.startsWith("---") || line.startsWith("+++") || line === "") continue;
      const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
      if (hunk) {
        a = Number(hunk[1]);
        b = Number(hunk[2]);
        out.push({ kind: "hunk", text: line });
      } else if (line.startsWith("+")) out.push({ kind: "add", text: line.slice(1), b: b++ });
      else if (line.startsWith("-")) out.push({ kind: "del", text: line.slice(1), a: a++ });
      else if (!line.startsWith("\\")) out.push({ kind: "ctx", text: line.slice(1), a: a++, b: b++ });
    }
    return out;
  }, [diff]);
  if (!diff) return <div className="notice">These revisions have the same content.</div>;
  return (
    <div className="diff" role="table" aria-label="Changes">
      {rows.map((r, i) => (
        <div key={i} className={`diff-row ${r.kind}`} role="row">
          <span className="diff-num" role="cell" aria-hidden="true">{r.a ?? ""}</span>
          <span className="diff-num" role="cell" aria-hidden="true">{r.b ?? ""}</span>
          <span className="diff-sign" role="cell" aria-hidden="true">{r.kind === "add" ? "+" : r.kind === "del" ? "−" : ""}</span>
          <span className="diff-text" role="cell">{r.text}</span>
        </div>
      ))}
    </div>
  );
}
