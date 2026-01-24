import { describe, expect, test } from "bun:test";
import { cn } from "../src/lib/utils";

describe("utils", () => {
  describe("cn", () => {
    test("merges class names", () => {
      expect(cn("foo", "bar")).toBe("foo bar");
    });

    test("handles conditional classes", () => {
      expect(cn("foo", false && "bar", "baz")).toBe("foo baz");
    });

    test("handles undefined values", () => {
      expect(cn("foo", undefined, "bar")).toBe("foo bar");
    });

    test("handles null values", () => {
      expect(cn("foo", null, "bar")).toBe("foo bar");
    });

    test("merges tailwind classes correctly", () => {
      expect(cn("p-4", "p-2")).toBe("p-2");
    });

    test("handles complex tailwind merges", () => {
      expect(cn("px-4 py-2", "px-2")).toBe("py-2 px-2");
    });

    test("handles array of classes", () => {
      expect(cn(["foo", "bar"])).toBe("foo bar");
    });

    test("handles object syntax", () => {
      expect(cn({ foo: true, bar: false, baz: true })).toBe("foo baz");
    });

    test("handles empty strings", () => {
      expect(cn("", "foo", "")).toBe("foo");
    });

    test("handles whitespace strings", () => {
      expect(cn("  ", "foo", "  ")).toBe("foo");
    });
  });
});
