import { afterEach, describe, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import {
  BlockSkeleton,
  CardStackSkeleton,
  GridSkeleton,
  OverviewPageSkeleton,
  StatsGridSkeleton,
  TableRowsSkeleton,
  UsersTableSkeleton,
} from "../src/components/shared/page-skeletons";

afterEach(() => {
  cleanup();
});

function countSkeletons() {
  return document.querySelectorAll(".animate-pulse").length;
}

describe("page skeletons", () => {
  test("renders grid-based skeleton helpers with requested counts", () => {
    render(
      <div>
        <GridSkeleton count={3} className="grid-cols-3" itemClassName="h-10" />
        <StatsGridSkeleton count={4} />
        <TableRowsSkeleton rows={2} />
        <CardStackSkeleton count={3} />
        <BlockSkeleton className="h-20" />
      </div>
    );

    expect(countSkeletons()).toBe(13);
    expect(document.querySelector(".grid-cols-3")).toBeTruthy();
  });

  test("renders overview page skeleton sections", () => {
    render(<OverviewPageSkeleton />);

    expect(screen.getByText("Overview")).toBeTruthy();
    expect(screen.getByText("Monitor automated issue fixing")).toBeTruthy();
    expect(countSkeletons()).toBe(14);
  });

  test("renders users table skeleton headers and row placeholders", () => {
    render(<UsersTableSkeleton rows={2} />);

    expect(screen.getByText("Name")).toBeTruthy();
    expect(screen.getByText("Email")).toBeTruthy();
    expect(screen.getByText("Role")).toBeTruthy();
    expect(screen.getByText("Created")).toBeTruthy();
    expect(screen.getByText("Actions")).toBeTruthy();
    expect(countSkeletons()).toBe(12);
  });
});
