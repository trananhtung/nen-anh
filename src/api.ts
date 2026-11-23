import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";

export type Target = "keep" | "webp" | "avif";
export type Level = "lossless" | "high" | "balanced" | "small";

export interface Options {
  target: Target;
  level: Level;
  stripMetadata: boolean;
}

export interface Item {
  path: string;
  subdir: string;
  base: string;
  size: number;
}

export interface Forecast {
  /** Size of all pictures now. */
  before: number;
  /** Predicted size at lossless, high, balanced and small; unreadable pictures count as not shrinking. */
  after: [number, number, number, number];
  sampled: number;
}

export type Report =
  | { state: "working" }
  | {
      state: "done";
      output: string;
      before: number;
      after: number;
      lossless: boolean;
      kept: boolean;
      score: number | null;
      note: string | null;
    }
  | { state: "failed"; message: string }
  | { state: "cancelled" };

export interface Progress {
  index: number;
  report: Report;
}

export type DropState = "over" | "leave";

export interface Api {
  scan(paths: string[]): Promise<Item[]>;
  /** Null when superseded, when compression is running, or when nothing could be read. */
  forecast(items: Item[], target: Target): Promise<Forecast | null>;
  start(items: Item[], outDir: string | null, options: Options): Promise<void>;
  cancel(): Promise<void>;
  reveal(path: string): Promise<void>;
  pickFiles(): Promise<string[]>;
  pickFolder(title: string): Promise<string | null>;
  onProgress(f: (p: Progress) => void): () => void;
  onFinished(f: () => void): () => void;
  onDrop(drop: (paths: string[]) => void, hover: (s: DropState) => void): () => void;
}

/** Turns Tauri's async unsubscribe into a synchronous one. */
function sync(p: Promise<() => void>): () => void {
  return () => void p.then((off) => off());
}

const native: Api = {
  scan: (paths) => invoke("scan", { paths }),
  forecast: (items, target) => invoke("forecast", { items, target }),
  start: (items, outDir, options) => invoke("start", { items, outDir, options }),
  cancel: () => invoke("cancel"),
  reveal: (path) => revealItemInDir(path),
  async pickFiles() {
    const picked = await open({
      multiple: true,
      title: "Chọn ảnh",
      filters: [{ name: "Ảnh", extensions: ["jpg", "jpeg", "jfif", "png", "webp"] }],
    });
    return picked ?? [];
  },
  pickFolder: (title) => open({ directory: true, title }),
  onProgress: (f) => sync(listen<Progress>("progress", (e) => f(e.payload))),
  onFinished: (f) => sync(listen("finished", () => f())),
  onDrop: (drop, hover) =>
    sync(
      getCurrentWebview().onDragDropEvent(({ payload }) => {
        if (payload.type === "drop") {
          hover("leave");
          drop(payload.paths);
        } else {
          hover(payload.type === "leave" ? "leave" : "over");
        }
      }),
    ),
};

/** Stand-in used only when the page is opened in a plain browser during development. */
function demo(): Api {
  let progress: (p: Progress) => void = () => {};
  let finished = () => {};
  let stop = false;
  const names = [
    ["IMG_2041.jpg", 4_812_000],
    ["Ảnh cưới Đà Lạt 014.jpg", 8_390_000],
    ["banner-trang-chủ.png", 2_210_000],
    ["logo.png", 83_000],
    ["hỏng.jpg", 120_000],
    ["sản-phẩm-áo-dài-màu-đỏ-tên-file-rất-dài-để-thử-cắt-chữ-0001.webp", 640_000],
  ] as const;
  const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
  return {
    scan: async (paths) =>
      paths.flatMap((p) => names.map(([n, size]) => ({ path: `${p}\\${n}`, subdir: "", base: p, size }))),
    async forecast(items, target) {
      await wait(900);
      const before = items.reduce((n, i) => n + i.size, 0);
      const k = target === "keep" ? [0.97, 0.7, 0.38, 0.24] : [0.95, 0.6, 0.3, 0.18];
      return { before, after: k.map((v) => v * before) as [number, number, number, number], sampled: items.length };
    },
    async start(items) {
      stop = false;
      void (async () => {
        for (const [index, it] of items.entries()) {
          if (stop) {
            progress({ index, report: { state: "cancelled" } });
            continue;
          }
          progress({ index, report: { state: "working" } });
          await wait(500);
          if (it.path.includes("hỏng")) {
            progress({ index, report: { state: "failed", message: "File ảnh bị hỏng, không đọc được" } });
            continue;
          }
          const kept = it.path.includes("logo");
          progress({
            index,
            report: {
              state: "done",
              output: it.path,
              before: it.size,
              after: kept ? it.size : Math.round(it.size * (0.25 + 0.1 * index)),
              lossless: false,
              kept,
              score: kept ? null : 86.4,
              note: kept ? "Ảnh đã tối ưu sẵn, giữ nguyên bản gốc" : null,
            },
          });
        }
        finished();
      })();
    },
    cancel: async () => {
      stop = true;
    },
    reveal: async () => {},
    pickFiles: async () => ["C:\\Ảnh"],
    pickFolder: async () => "D:\\Kết quả",
    onProgress: (f) => {
      progress = f;
      return () => {};
    },
    onFinished: (f) => {
      finished = f;
      return () => {};
    },
    onDrop: () => () => {},
  };
}

export const api: Api = import.meta.env.DEV && !isTauri() ? demo() : native;
