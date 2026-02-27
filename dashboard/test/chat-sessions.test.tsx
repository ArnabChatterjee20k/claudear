import { afterEach, describe, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ChatSessions } from "../src/components/chat/ChatSessions";
import type { ChatSession } from "../src/lib/chat";

afterEach(() => {
  cleanup();
});

const makeSessions = (count: number): ChatSession[] =>
  Array.from({ length: count }, (_, i) => ({
    id: `s${i + 1}-abcde-fgh-ijkl-mnop`,
    created_at: "2024-06-01T10:00:00Z",
    updated_at: `2024-06-0${i + 1}T12:00:00Z`,
  }));

describe("ChatSessions", () => {
  test("renders New Chat button", () => {
    const onNewSession = mock(() => {});
    render(
      <ChatSessions
        sessions={[]}
        activeSessionId={null}
        onSelectSession={mock(() => {})}
        onNewSession={onNewSession}
        onDeleteSession={mock(() => {})}
      />
    );

    expect(screen.getByText("New Chat")).toBeTruthy();
  });

  test("calls onNewSession when New Chat is clicked", () => {
    const onNewSession = mock(() => {});
    render(
      <ChatSessions
        sessions={[]}
        activeSessionId={null}
        onSelectSession={mock(() => {})}
        onNewSession={onNewSession}
        onDeleteSession={mock(() => {})}
      />
    );

    fireEvent.click(screen.getByText("New Chat"));
    expect(onNewSession).toHaveBeenCalledTimes(1);
  });

  test("shows empty state when no sessions", () => {
    render(
      <ChatSessions
        sessions={[]}
        activeSessionId={null}
        onSelectSession={mock(() => {})}
        onNewSession={mock(() => {})}
        onDeleteSession={mock(() => {})}
      />
    );

    expect(screen.getByText("No conversations yet")).toBeTruthy();
  });

  test("renders session list with truncated IDs", () => {
    const sessions = makeSessions(2);
    render(
      <ChatSessions
        sessions={sessions}
        activeSessionId={null}
        onSelectSession={mock(() => {})}
        onNewSession={mock(() => {})}
        onDeleteSession={mock(() => {})}
      />
    );

    // IDs are truncated to first 8 chars + "..."
    // s1-abcde → "s1-abcde..." and s2-abcde → "s2-abcde..."
    expect(screen.getByText("s1-abcde...")).toBeTruthy();
    expect(screen.getByText("s2-abcde...")).toBeTruthy();
  });

  test("calls onSelectSession with session id on click", () => {
    const sessions = makeSessions(1);
    const onSelectSession = mock(() => {});
    render(
      <ChatSessions
        sessions={sessions}
        activeSessionId={null}
        onSelectSession={onSelectSession}
        onNewSession={mock(() => {})}
        onDeleteSession={mock(() => {})}
      />
    );

    // Click the session row (find by truncated ID text: "s1-abcde...")
    fireEvent.click(screen.getByText("s1-abcde..."));
    expect(onSelectSession).toHaveBeenCalledWith(sessions[0].id);
  });

  test("calls onDeleteSession on delete button click", () => {
    const sessions = makeSessions(1);
    const onDeleteSession = mock(() => {});
    const onSelectSession = mock(() => {});
    render(
      <ChatSessions
        sessions={sessions}
        activeSessionId={null}
        onSelectSession={onSelectSession}
        onNewSession={mock(() => {})}
        onDeleteSession={onDeleteSession}
      />
    );

    const deleteBtn = screen.getByTitle("Delete session");
    fireEvent.click(deleteBtn);

    expect(onDeleteSession).toHaveBeenCalledWith(sessions[0].id);
    // Delete should not trigger select (stopPropagation)
    expect(onSelectSession).not.toHaveBeenCalled();
  });

  test("highlights active session", () => {
    const sessions = makeSessions(2);
    const { container } = render(
      <ChatSessions
        sessions={sessions}
        activeSessionId={sessions[0].id}
        onSelectSession={mock(() => {})}
        onNewSession={mock(() => {})}
        onDeleteSession={mock(() => {})}
      />
    );

    // The active session row should have the primary highlight class
    const sessionRows = container.querySelectorAll(".cursor-pointer");
    expect(sessionRows.length).toBe(2);
    expect(sessionRows[0].className).toContain("bg-primary/10");
    expect(sessionRows[1].className).not.toContain("bg-primary/10");
  });

  test("renders multiple sessions", () => {
    const sessions = makeSessions(3);
    const { container } = render(
      <ChatSessions
        sessions={sessions}
        activeSessionId={null}
        onSelectSession={mock(() => {})}
        onNewSession={mock(() => {})}
        onDeleteSession={mock(() => {})}
      />
    );

    const sessionRows = container.querySelectorAll(".cursor-pointer");
    expect(sessionRows.length).toBe(3);
  });
});
