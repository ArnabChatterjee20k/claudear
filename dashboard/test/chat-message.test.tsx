import { afterEach, describe, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ChatMessage } from "../src/components/chat/ChatMessage";
import type { ChatMessage as ChatMessageType } from "../src/lib/chat";

afterEach(() => {
  cleanup();
});

describe("ChatMessage", () => {
  test("renders user message content", () => {
    const msg: ChatMessageType = { role: "user", content: "Hello world" };
    render(<ChatMessage message={msg} />);

    expect(screen.getByText("Hello world")).toBeTruthy();
  });

  test("renders assistant message content", () => {
    const msg: ChatMessageType = { role: "assistant", content: "Here is the answer" };
    render(<ChatMessage message={msg} />);

    expect(screen.getByText("Here is the answer")).toBeTruthy();
  });

  test("shows streaming dots when content is empty and streaming", () => {
    const msg: ChatMessageType = { role: "assistant", content: "" };
    const { container } = render(<ChatMessage message={msg} isStreaming />);

    // Three animated dots
    const dots = container.querySelectorAll(".animate-pulse");
    expect(dots.length).toBe(3);
  });

  test("shows streaming cursor when streaming with content", () => {
    const msg: ChatMessageType = { role: "assistant", content: "Partial resp" };
    const { container } = render(<ChatMessage message={msg} isStreaming />);

    expect(screen.getByText("Partial resp")).toBeTruthy();
    // The pipe cursor
    const cursor = container.querySelector(".animate-pulse");
    expect(cursor).toBeTruthy();
    expect(cursor?.textContent).toBe("|");
  });

  test("does not show streaming cursor when not streaming", () => {
    const msg: ChatMessageType = { role: "assistant", content: "Done" };
    const { container } = render(<ChatMessage message={msg} isStreaming={false} />);

    expect(screen.getByText("Done")).toBeTruthy();
    const cursor = container.querySelector(".animate-pulse");
    expect(cursor).toBeNull();
  });

  test("renders inline code with backticks", () => {
    const msg: ChatMessageType = { role: "assistant", content: "Use `foo()` here" };
    render(<ChatMessage message={msg} />);

    expect(screen.getByText("foo()")).toBeTruthy();
    const codeEl = screen.getByText("foo()");
    expect(codeEl.tagName.toLowerCase()).toBe("code");
  });

  test("renders fenced code block", () => {
    const msg: ChatMessageType = {
      role: "assistant",
      content: "Here is code:\n```rust\nfn main() {}\n```",
    };
    render(<ChatMessage message={msg} />);

    expect(screen.getByText("rust")).toBeTruthy();
    expect(screen.getByText("fn main() {}")).toBeTruthy();
    expect(screen.getByTitle("Copy code")).toBeTruthy();
  });

  test("renders code block without language tag", () => {
    const msg: ChatMessageType = {
      role: "assistant",
      content: "```\nhello\n```",
    };
    render(<ChatMessage message={msg} />);

    expect(screen.getByText("code")).toBeTruthy();
    expect(screen.getByText("hello")).toBeTruthy();
  });

  // --- Sources panel ---

  test("does not show sources for user messages", () => {
    const msg: ChatMessageType = {
      role: "user",
      content: "test",
      sources: [{ file_path: "src/main.rs", start_line: 1, end_line: 10, similarity: 0.9 }],
    };
    render(<ChatMessage message={msg} />);

    expect(screen.queryByText(/source/)).toBeNull();
  });

  test("does not show sources panel when no sources", () => {
    const msg: ChatMessageType = { role: "assistant", content: "answer", sources: [] };
    render(<ChatMessage message={msg} />);

    expect(screen.queryByText(/source/)).toBeNull();
  });

  test("shows collapsed sources count for assistant messages", () => {
    const msg: ChatMessageType = {
      role: "assistant",
      content: "answer",
      sources: [
        { file_path: "src/main.rs", start_line: 1, end_line: 10, similarity: 0.9 },
        { file_path: "src/lib.rs", start_line: 5, end_line: 20, similarity: 0.85 },
      ],
    };
    render(<ChatMessage message={msg} />);

    expect(screen.getByText("2 sources")).toBeTruthy();
  });

  test("shows singular 'source' for single source", () => {
    const msg: ChatMessageType = {
      role: "assistant",
      content: "answer",
      sources: [
        { file_path: "src/main.rs", start_line: 1, end_line: 10, similarity: 0.9 },
      ],
    };
    render(<ChatMessage message={msg} />);

    expect(screen.getByText("1 source")).toBeTruthy();
  });

  test("expands sources panel on click", () => {
    const msg: ChatMessageType = {
      role: "assistant",
      content: "answer",
      sources: [
        { file_path: "src/main.rs", start_line: 1, end_line: 10, symbol_name: "main", similarity: 0.95 },
      ],
    };
    render(<ChatMessage message={msg} />);

    // Click the sources toggle
    fireEvent.click(screen.getByText("1 source"));

    // Source details should now be visible
    expect(screen.getByText("src/main.rs")).toBeTruthy();
    expect(screen.getByText("(main)")).toBeTruthy();
    expect(screen.getByText("95%")).toBeTruthy();
  });

  test("collapses sources on second click", () => {
    const msg: ChatMessageType = {
      role: "assistant",
      content: "answer",
      sources: [
        { file_path: "src/lib.rs", start_line: 5, end_line: 20, similarity: 0.8 },
      ],
    };
    render(<ChatMessage message={msg} />);

    const toggle = screen.getByText("1 source");
    fireEvent.click(toggle);
    expect(screen.getByText("src/lib.rs")).toBeTruthy();

    fireEvent.click(toggle);
    expect(screen.queryByText("src/lib.rs")).toBeNull();
  });

  test("copy code button triggers clipboard write", () => {
    const writeTextMock = mock(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: writeTextMock },
      writable: true,
      configurable: true,
    });

    const msg: ChatMessageType = {
      role: "assistant",
      content: "```rust\nfn main() {}\n```",
    };
    render(<ChatMessage message={msg} />);

    const copyBtn = screen.getByTitle("Copy code");
    fireEvent.click(copyBtn);

    expect(writeTextMock).toHaveBeenCalledWith("fn main() {}");
  });

  test("source without symbol_name does not show parenthesized name", () => {
    const msg: ChatMessageType = {
      role: "assistant",
      content: "answer",
      sources: [
        { file_path: "src/foo.rs", start_line: 1, end_line: 5, similarity: 0.7 },
      ],
    };
    render(<ChatMessage message={msg} />);

    fireEvent.click(screen.getByText("1 source"));
    expect(screen.getByText("src/foo.rs")).toBeTruthy();
    expect(screen.getByText("70%")).toBeTruthy();
    // No parenthesized symbol name
    expect(screen.queryByText(/\(.*\)/)).toBeNull();
  });
});
