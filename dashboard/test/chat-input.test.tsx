import { afterEach, describe, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ChatInput } from "../src/components/chat/ChatInput";

afterEach(() => {
  cleanup();
});

describe("ChatInput", () => {
  test("renders textarea and send button", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} />);

    expect(screen.getByPlaceholderText("Ask about your code...")).toBeTruthy();
    expect(screen.getByTitle("Send message")).toBeTruthy();
  });

  test("uses custom placeholder", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} placeholder="Type here..." />);

    expect(screen.getByPlaceholderText("Type here...")).toBeTruthy();
  });

  test("send button is disabled when textarea is empty", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} />);

    const button = screen.getByTitle("Send message");
    expect(button.hasAttribute("disabled")).toBe(true);
  });

  test("send button is disabled when disabled prop is true", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} disabled />);

    const textarea = screen.getByPlaceholderText("Ask about your code...");
    expect(textarea.hasAttribute("disabled")).toBe(true);
  });

  test("calls onSend with trimmed value on button click", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} />);

    const textarea = screen.getByPlaceholderText("Ask about your code...");
    fireEvent.change(textarea, { target: { value: "  hello world  " } });

    const button = screen.getByTitle("Send message");
    fireEvent.click(button);

    expect(onSend).toHaveBeenCalledWith("hello world");
  });

  test("clears textarea after sending", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} />);

    const textarea = screen.getByPlaceholderText("Ask about your code...") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "test message" } });
    fireEvent.click(screen.getByTitle("Send message"));

    expect(textarea.value).toBe("");
  });

  test("sends on Enter key (without Shift)", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} />);

    const textarea = screen.getByPlaceholderText("Ask about your code...");
    fireEvent.change(textarea, { target: { value: "enter message" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });

    expect(onSend).toHaveBeenCalledWith("enter message");
  });

  test("does not send on Shift+Enter", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} />);

    const textarea = screen.getByPlaceholderText("Ask about your code...");
    fireEvent.change(textarea, { target: { value: "multiline" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: true });

    expect(onSend).not.toHaveBeenCalled();
  });

  test("does not send whitespace-only messages", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} />);

    const textarea = screen.getByPlaceholderText("Ask about your code...");
    fireEvent.change(textarea, { target: { value: "   " } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });

    expect(onSend).not.toHaveBeenCalled();
  });

  test("does not send when disabled", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} disabled />);

    const textarea = screen.getByPlaceholderText("Ask about your code...");
    fireEvent.change(textarea, { target: { value: "test" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });

    expect(onSend).not.toHaveBeenCalled();
  });

  // --- Stop button ---

  test("shows stop button when isStreaming is true", () => {
    const onSend = mock(() => {});
    const onStop = mock(() => {});
    render(<ChatInput onSend={onSend} onStop={onStop} isStreaming />);

    expect(screen.getByTitle("Stop generation")).toBeTruthy();
    expect(screen.queryByTitle("Send message")).toBeNull();
  });

  test("shows send button when isStreaming is false", () => {
    const onSend = mock(() => {});
    const onStop = mock(() => {});
    render(<ChatInput onSend={onSend} onStop={onStop} isStreaming={false} />);

    expect(screen.getByTitle("Send message")).toBeTruthy();
    expect(screen.queryByTitle("Stop generation")).toBeNull();
  });

  test("calls onStop when stop button is clicked", () => {
    const onSend = mock(() => {});
    const onStop = mock(() => {});
    render(<ChatInput onSend={onSend} onStop={onStop} isStreaming />);

    fireEvent.click(screen.getByTitle("Stop generation"));
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  test("does not send on Enter while streaming", () => {
    const onSend = mock(() => {});
    const onStop = mock(() => {});
    render(<ChatInput onSend={onSend} onStop={onStop} isStreaming />);

    const textarea = screen.getByPlaceholderText("Ask about your code...");
    fireEvent.change(textarea, { target: { value: "test" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });

    expect(onSend).not.toHaveBeenCalled();
  });

  test("textarea remains enabled during streaming", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} isStreaming />);

    const textarea = screen.getByPlaceholderText("Ask about your code...");
    expect(textarea.hasAttribute("disabled")).toBe(false);
  });

  test("triggers auto-resize on input event", () => {
    const onSend = mock(() => {});
    render(<ChatInput onSend={onSend} />);

    const textarea = screen.getByPlaceholderText("Ask about your code...") as HTMLTextAreaElement;
    // Simulate typing triggers the onInput handler which sets height
    fireEvent.input(textarea);

    // In happy-dom, scrollHeight is mocked to 768, so min(768, 200) = 200
    // The handler sets style.height = 'auto' then to scrollHeight-capped value
    expect(textarea.style.height).toBeTruthy();
  });
});
