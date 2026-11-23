import { type CSSProperties, useCallback, useEffect, useRef, useState } from "react";
import { type Forecast, type Item, type Level, type Options, type Report, type Target, api } from "./api";
import { bytes, fileName, range, roughly, saved } from "./format";
import { SAMPLE_COUNT, TYPICAL } from "./typical";

interface Row {
  item: Item;
  report: Report | null;
}

const TARGETS: { value: Target; label: string; hint: string }[] = [
  { value: "keep", label: "Giữ nguyên", hint: "JPG vẫn là JPG, PNG vẫn là PNG. Mở được ở mọi nơi." },
  { value: "webp", label: "WebP", hint: "Thường nhẹ hơn JPG, hợp cho website." },
  { value: "avif", label: "AVIF", hint: "Thường nhẹ nhất nhưng nén chậm. Phần mềm cũ có thể không mở được." },
];

const LEVELS: { value: Level; label: string; hint: string }[] = [
  { value: "lossless", label: "Không mất dữ liệu", hint: "Giống bản gốc từng điểm ảnh. Giảm ít nhất." },
  { value: "high", label: "Chất lượng cao", hint: "Gần như không thể phân biệt bằng mắt." },
  { value: "balanced", label: "Cân bằng", hint: "Nhẹ hơn nhiều, rất khó nhận ra khác biệt." },
  { value: "small", label: "Nhẹ nhất", hint: "Có thể thấy khác biệt khi phóng to." },
];

const isFinal = (r: Report | null) => r?.state === "done" || r?.state === "failed";

function load<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? { ...fallback, ...JSON.parse(raw) } : fallback;
  } catch {
    return fallback;
  }
}

export function App() {
  const [rows, setRows] = useState<Row[]>([]);
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [hover, setHover] = useState(false);
  const [message, setMessage] = useState("");
  const [outDir, setOutDir] = useState<string | null>(null);
  const [options, setOptions] = useState<Options>(() =>
    load("options", { target: "keep", level: "balanced", stripMetadata: false }),
  );
  const latest = useRef(rows);
  latest.current = rows;
  /** Paths of the current run, in the order the backend reports them. */
  const queue = useRef<string[]>([]);
  const busy = useRef(false);
  const [forecast, setForecast] = useState<Forecast | "working" | null>(null);

  useEffect(() => localStorage.setItem("options", JSON.stringify(options)), [options]);

  const add = useCallback(async (paths: string[]) => {
    if (paths.length === 0) return;
    if (busy.current) {
      setMessage("Đang nén. Hãy thêm ảnh sau khi xong.");
      return;
    }
    const found = await api.scan(paths);
    setMessage(found.length === 0 ? "Không tìm thấy ảnh JPG, PNG hoặc WebP nào." : "");
    setRows((old) => {
      const known = new Set(old.map((r) => r.item.path));
      return [...old, ...found.filter((i) => !known.has(i.path)).map((item) => ({ item, report: null }))];
    });
  }, []);

  useEffect(() => {
    const offs = [
      api.onDrop(add, (s) => setHover(s === "over")),
      api.onProgress(({ index, report }) => {
        const path = queue.current[index];
        setRows((old) => old.map((r) => (r.item.path === path ? { ...r, report } : r)));
      }),
      api.onFinished(() => {
        busy.current = false;
        setRunning(false);
        setStopping(false);
      }),
    ];
    return () => offs.forEach((off) => off());
  }, [add]);

  const change = (patch: Partial<Options>) => {
    setOptions((o) => {
      const next = { ...o, ...patch };
      if (next.target === "avif" && next.level === "lossless") next.level = "high";
      return next;
    });
    // Earlier results no longer match the settings.
    setRows((old) => old.map((r) => ({ ...r, report: null })));
  };

  const list = rows.map((r) => r.item.path).join("\n");
  /** What the known forecast was computed for. */
  const forecastKey = useRef("");
  // Leave the processor to the real work; ask again only while something is waiting to be compressed.
  const waiting = !running && rows.some((r) => !isFinal(r.report));
  useEffect(() => {
    const items = latest.current.map((r) => r.item);
    if (list === "" || items.length === 0) {
      forecastKey.current = "";
      setForecast(null);
      return;
    }
    const key = `${options.target}\n${list}`;
    if (forecastKey.current === key) return;
    if (!waiting) {
      setForecast(null);
      return;
    }
    forecastKey.current = "";
    let stale = false;
    setForecast("working");
    const timer = setTimeout(async () => {
      const result = await api.forecast(items, options.target).catch(() => null);
      if (stale) return;
      if (result) forecastKey.current = key;
      setForecast(result);
    }, 250);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [list, options.target, waiting]);

  const pending = rows.filter((r) => !isFinal(r.report));
  const done = rows.flatMap((r) => (r.report?.state === "done" ? [r.report] : []));
  const failed = rows.filter((r) => r.report?.state === "failed").length;
  const before = done.reduce((n, r) => n + r.before, 0);
  const after = done.reduce((n, r) => n + r.after, 0);
  const total = rows.reduce((n, r) => n + r.item.size, 0);

  const start = async () => {
    const items = pending.map((r) => r.item);
    if (items.length === 0 || busy.current) return;
    busy.current = true;
    queue.current = items.map((i) => i.path);
    setMessage("");
    setRunning(true);
    try {
      await api.start(items, outDir, options);
    } catch (e) {
      busy.current = false;
      setRunning(false);
      setMessage(String(e));
    }
  };

  const stop = () => {
    setStopping(true);
    void api.cancel();
  };

  const clear = () => {
    setRows([]);
    setMessage("");
  };

  const pickFiles = async () => add(await api.pickFiles());
  const pickFolder = async () => {
    const dir = await api.pickFolder("Chọn thư mục ảnh");
    if (dir) await add([dir]);
  };
  const pickOutput = async () => {
    const dir = await api.pickFolder("Chọn nơi lưu ảnh đã nén");
    if (dir) setOutDir(dir);
  };
  const reveal = async (path: string) => {
    try {
      await api.reveal(path);
    } catch {
      setMessage("Không mở được thư mục. File có thể đã bị chuyển đi.");
    }
  };

  const count = rows.length;
  const known = forecast !== null && forecast !== "working" ? forecast : null;
  const levelIndex = LEVELS.findIndex((l) => l.value === options.level);
  const expected = known?.after[levelIndex];
  // The forecast covers readable pictures only; apply its ratio to the whole list.
  const ratio = known && expected !== undefined && known.before > 0 ? expected / known.before : null;

  const promise = (index: number): string => {
    const level = LEVELS[index];
    if (!level || (level.value === "lossless" && options.target === "avif")) return "";
    if (forecast === "working") return "đang tính…";
    if (known) return roughly(known.before, known.after[index] ?? known.before);
    const typical = TYPICAL[options.target][index];
    return count === 0 && typical ? range(typical) : "";
  };

  const first = done[0];

  return (
    <div className="app" data-hover={hover || undefined}>
      <main className="work">
        {count === 0 ? (
          <section className="drop">
            <div className="drop-art" aria-hidden="true">
              <i />
              <i />
            </div>
            <h1>Kéo ảnh hoặc thư mục vào đây</h1>
            <p>JPG, PNG, WebP. Ảnh được xử lý ngay trên máy, bản gốc không bị thay đổi.</p>
            <div className="actions">
              <button type="button" className="primary" onClick={pickFiles}>
                Chọn ảnh
              </button>
              <button type="button" onClick={pickFolder}>
                Chọn thư mục
              </button>
            </div>
          </section>
        ) : (
          <>
            <header className="bar">
              <h1>{count} ảnh</h1>
              <span className="muted num">{bytes(total)}</span>
              <span className="grow" />
              <button type="button" onClick={pickFiles} disabled={running}>
                Thêm ảnh
              </button>
              <button type="button" onClick={pickFolder} disabled={running}>
                Thêm thư mục
              </button>
              <button type="button" onClick={clear} disabled={running}>
                Xoá danh sách
              </button>
            </header>
            <ul className="list">
              {rows.map((r) => (
                <RowView key={r.item.path} row={r} onReveal={reveal} />
              ))}
            </ul>
          </>
        )}
        {message && (
          <p className="message" role="status">
            {message}
          </p>
        )}
      </main>

      <aside className="panel">
        <fieldset disabled={running}>
          <legend>Định dạng</legend>
          <div className="segments">
            {TARGETS.map((t) => (
              <label key={t.value}>
                <input
                  type="radio"
                  name="target"
                  checked={options.target === t.value}
                  onChange={() => change({ target: t.value })}
                />
                <span>{t.label}</span>
              </label>
            ))}
          </div>
          <p className="hint">{TARGETS.find((t) => t.value === options.target)?.hint}</p>
        </fieldset>

        <fieldset disabled={running}>
          <legend>Mức nén và tỉ lệ giảm</legend>
          {LEVELS.map((l, i) => (
            <label key={l.value} className="choice">
              <input
                type="radio"
                name="level"
                checked={options.level === l.value}
                disabled={l.value === "lossless" && options.target === "avif"}
                onChange={() => change({ level: l.value })}
              />
              <span>
                <b>
                  {l.label}
                  <em className="num">{promise(i)}</em>
                </b>
                <small>{l.hint}</small>
              </span>
            </label>
          ))}
          <p className="hint">
            {count === 0
              ? `Mức giảm thường gặp, đo trên ${SAMPLE_COUNT} ảnh mẫu. Ảnh đã nén sẵn giảm ít hơn.`
              : known
                ? `Ước tính từ ${known.sampled}/${count} ảnh của bạn. Kết quả thật có thể khác.`
                : forecast === "working"
                  ? "Đang ước tính từ ảnh của bạn…"
                  : running || done.length > 0
                    ? "Xem kết quả thật ở danh sách bên trái."
                    : "Không ước tính được từ các ảnh này."}
          </p>
        </fieldset>

        <fieldset disabled={running}>
          <legend>Tuỳ chọn</legend>
          <label className="choice">
            <input
              type="checkbox"
              checked={options.stripMetadata}
              onChange={(e) => change({ stripMetadata: e.target.checked })}
            />
            <span>
              <b>Xoá thông tin cá nhân</b>
              <small>Vị trí GPS, máy ảnh, ngày chụp.</small>
            </span>
          </label>
          <div className="output">
            <span className="output-label">Lưu vào</span>
            <button type="button" className="path" onClick={pickOutput} title={outDir ?? undefined}>
              {outDir ?? "Thư mục “anh-nen” cạnh ảnh gốc"}
            </button>
            {outDir && (
              <button type="button" className="link" onClick={() => setOutDir(null)}>
                Dùng mặc định
              </button>
            )}
          </div>
        </fieldset>

        <footer className="summary" aria-live="polite">
          {done.length > 0 ? (
            <>
              <strong className="num">−{saved(before, after)}%</strong>
              <span className="num">
                {bytes(before)} → {bytes(after)}
              </span>
              <span className="muted">
                {running ? `Xong ${done.length + failed}/${count} ảnh` : `Đã xong ${done.length} ảnh`}
                {failed > 0 && `, ${failed} lỗi`}
              </span>
            </>
          ) : !running && ratio !== null ? (
            <>
              <span className="muted">Ước tính giảm</span>
              <strong className="num">{roughly(1, ratio)}</strong>
              <span className="num">
                {bytes(total)} → khoảng {bytes(Math.round(total * ratio))}
              </span>
            </>
          ) : (
            <span className="muted">
              {running ? "Đang nén…" : count > 0 ? "Sẵn sàng nén" : "Chưa có ảnh nào"}
            </span>
          )}
          {running ? (
            <button type="button" className="big" onClick={stop} disabled={stopping}>
              {stopping ? "Đang dừng…" : "Dừng"}
            </button>
          ) : pending.length > 0 || !first ? (
            <button type="button" className="big primary" onClick={start} disabled={pending.length === 0}>
              {pending.length > 0 ? `Nén ${pending.length} ảnh` : "Nén ảnh"}
            </button>
          ) : (
            <button type="button" className="big primary" onClick={() => reveal(first.output)}>
              Mở thư mục kết quả
            </button>
          )}
        </footer>
      </aside>
    </div>
  );
}

function RowView({ row, onReveal }: { row: Row; onReveal: (path: string) => void }) {
  const { item, report } = row;
  const done = report?.state === "done" ? report : null;
  const ratio = done && done.before > 0 ? done.after / done.before : 1;
  const name = fileName(item.path);

  let status = "Chờ nén";
  if (report?.state === "working") status = "Đang nén…";
  if (report?.state === "cancelled") status = "Đã dừng";
  if (report?.state === "failed") status = report.message;
  if (done) {
    if (done.note) status = done.note;
    else if (done.lossless) status = "Giống bản gốc từng điểm ảnh";
    else if (done.score !== null) status = `Độ giống bản gốc ${Math.floor(done.score)}/100`;
    else status = "Đã nén";
  }

  return (
    <li className="row" data-state={report?.state ?? "waiting"} data-kept={done?.kept || undefined}>
      <div className="row-main">
        <span className="name" title={item.path}>
          {name}
        </span>
        <span className="status" title={status}>
          {status}
        </span>
        <div className="size-bar" aria-hidden="true">
          <i style={{ "--ratio": ratio } as CSSProperties} />
        </div>
      </div>
      <div className="figures num">
        <span>{done ? `${bytes(done.before)} → ${bytes(done.after)}` : bytes(item.size)}</span>
        <b>{done ? (done.kept ? "Giữ nguyên" : `−${saved(done.before, done.after)}%`) : ""}</b>
      </div>
      <button
        type="button"
        className="link"
        disabled={!done}
        onClick={() => done && onReveal(done.output)}
        aria-label={`Xem ${name} trong thư mục`}
      >
        Xem file
      </button>
    </li>
  );
}
