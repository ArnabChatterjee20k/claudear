import { afterEach, describe, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Modal } from "../src/components/shared/modal";

afterEach(() => {
  cleanup();
  document.body.style.overflow = "";
});

describe("Modal", () => {
  test("returns null when closed", () => {
    const onClose = mock(() => {});
    render(
      <Modal open={false} onClose={onClose} title="Hidden">
        <div>Content</div>
      </Modal>
    );

    expect(screen.queryByText("Content")).toBeNull();
    expect(document.body.style.overflow).toBe("");
  });

  test("renders in a portal and restores body overflow when closed", () => {
    const onClose = mock(() => {});
    const { rerender } = render(
      <Modal open onClose={onClose} title="Details">
        <div>Body content</div>
      </Modal>
    );

    expect(screen.getByText("Details")).toBeTruthy();
    expect(screen.getByText("Body content")).toBeTruthy();
    expect(document.body.style.overflow).toBe("hidden");

    rerender(
      <Modal open={false} onClose={onClose} title="Details">
        <div>Body content</div>
      </Modal>
    );

    expect(document.body.style.overflow).toBe("");
  });

  test("closes via backdrop, close button, and Escape key", () => {
    const onClose = mock(() => {});
    render(
      <Modal open onClose={onClose}>
        <div>Body content</div>
      </Modal>
    );

    const closeButton = screen.getByRole("button");
    fireEvent.click(closeButton);

    const portalRoot = Array.from(document.body.children).find((el) =>
      typeof el.className === "string" && el.className.includes("fixed inset-0 z-50")
    ) as HTMLElement | undefined;
    const backdrop = portalRoot?.firstElementChild as HTMLElement | null;
    expect(backdrop).toBeTruthy();
    if (backdrop) {
      fireEvent.click(backdrop);
    }

    fireEvent.keyDown(document, { key: "Escape" });

    expect(onClose).toHaveBeenCalledTimes(3);
  });
});
