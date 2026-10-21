// Parses spreadsheets off the main thread. SheetJS from npm has open advisories for crafted
// files (prototype pollution, ReDoS), so parsing stays in this worker: a polluted prototype
// cannot reach the page, and the page terminates the worker if it hangs.
import * as XLSX from "xlsx";

export interface SheetData {
  name: string;
  rows: string[][];
  totalRows: number;
  totalCols: number;
}

const MAX_ROWS = 2000;
const MAX_COLS = 100;

self.onmessage = (e: MessageEvent<{ data: ArrayBuffer | string; csv?: "," | "\t" }>) => {
  try {
    const { data, csv } = e.data;
    const book =
      typeof data === "string"
        ? XLSX.read(data, { type: "string", FS: csv ?? ",", dense: true })
        : XLSX.read(new Uint8Array(data), { type: "array", dense: true, cellDates: true });
    const sheets: SheetData[] = book.SheetNames.map((name) => {
      const sheet = book.Sheets[name];
      if (!sheet) return { name, rows: [], totalRows: 0, totalCols: 0 };
      const all = XLSX.utils.sheet_to_json<unknown[]>(sheet, { header: 1, raw: false, defval: "", blankrows: true });
      const totalCols = all.reduce((m, r) => Math.max(m, r.length), 0);
      const rows = all.slice(0, MAX_ROWS).map((r) => r.slice(0, MAX_COLS).map((c) => (c == null ? "" : String(c))));
      return { name, rows, totalRows: all.length, totalCols };
    });
    self.postMessage({ ok: true, sheets });
  } catch (err) {
    self.postMessage({ ok: false, error: err instanceof Error ? err.message : String(err) });
  }
};
