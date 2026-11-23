import { expect, test } from "vitest";
import { bytes, fileName, range, roughly, saved } from "./format";

test("bytes", () => {
  expect(bytes(512)).toBe("512 B");
  expect(bytes(1536)).toBe("1,5 KB");
  expect(bytes(5 * 1024 * 1024)).toBe("5 MB");
});

test("saved never overstates", () => {
  expect(saved(1000, 999)).toBe(0);
  expect(saved(1000, 335)).toBe(66);
  expect(saved(0, 0)).toBe(0);
  expect(saved(100, 120)).toBe(0);
});

test("fileName handles both separators", () => {
  expect(fileName("C:\\Ảnh\\cưới.jpg")).toBe("cưới.jpg");
  expect(fileName("/a/b.png")).toBe("b.png");
});

test("roughly rounds down", () => {
  expect(roughly(1000, 261)).toBe("khoảng 70%");
  expect(roughly(1000, 960)).toBe("dưới 5%");
  expect(roughly(1000, 1000)).toBe("dưới 5%");
});

test("range", () => {
  expect(range([0, 3])).toBe("dưới 5%");
  expect(range([10, 60])).toBe("10–60%");
});
