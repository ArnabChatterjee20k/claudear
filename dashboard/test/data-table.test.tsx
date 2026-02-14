import { describe, test, expect, afterEach } from "bun:test";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { DataTable, type Column } from "../src/components/shared/data-table";

interface TestItem {
  id: number;
  name: string;
  score: number;
}

const columns: Column<TestItem>[] = [
  { key: "name", header: "Name", render: (row) => <span>{row.name}</span> },
  {
    key: "score",
    header: "Score",
    render: (row) => <span>{row.score}</span>,
    sortable: true,
  },
];

const data: TestItem[] = [
  { id: 1, name: "Alice", score: 95 },
  { id: 2, name: "Bob", score: 87 },
];

describe("DataTable", () => {
  afterEach(() => {
    cleanup();
  });

  test("renders empty state when data is empty", () => {
    render(
      <DataTable columns={columns} data={[]} keyFn={(row) => row.id} />
    );
    expect(screen.getByText("No data available")).toBeDefined();
  });

  test("renders empty state with custom emptyMessage", () => {
    render(
      <DataTable
        columns={columns}
        data={[]}
        keyFn={(row) => row.id}
        emptyMessage="Nothing here"
      />
    );
    expect(screen.getByText("Nothing here")).toBeDefined();
  });

  test("renders table with data rows", () => {
    render(
      <DataTable columns={columns} data={data} keyFn={(row) => row.id} />
    );
    expect(screen.getByText("Alice")).toBeDefined();
    expect(screen.getByText("Bob")).toBeDefined();
    expect(screen.getByText("95")).toBeDefined();
    expect(screen.getByText("87")).toBeDefined();
  });

  test("renders column headers", () => {
    render(
      <DataTable columns={columns} data={data} keyFn={(row) => row.id} />
    );
    expect(screen.getByText("Name")).toBeDefined();
    expect(screen.getByText("Score")).toBeDefined();
  });

  test("sortable column shows cursor-pointer styling", () => {
    render(
      <DataTable columns={columns} data={data} keyFn={(row) => row.id} />
    );
    const scoreHeader = screen.getByText("Score");
    expect(scoreHeader.className).toContain("cursor-pointer");
  });

  test("clicking sortable header shows sort indicator (desc initially)", () => {
    render(
      <DataTable columns={columns} data={data} keyFn={(row) => row.id} />
    );
    const scoreHeader = screen.getByText("Score");
    fireEvent.click(scoreHeader);
    // After click, the header text should include the down arrow for desc
    const th = scoreHeader.closest("th");
    expect(th?.textContent).toContain("\u2193");
  });

  test("clicking same header again toggles to asc", () => {
    render(
      <DataTable columns={columns} data={data} keyFn={(row) => row.id} />
    );
    const scoreHeader = screen.getByText("Score");
    // First click: sets sortKey to 'score', dir = desc
    fireEvent.click(scoreHeader);
    // Second click: toggles to asc
    fireEvent.click(scoreHeader.closest("th")!);
    const th = scoreHeader.closest("th");
    expect(th?.textContent).toContain("\u2191");
  });

  test("clicking a different sortable header resets to desc", () => {
    // Use a third sortable column to test switching between sortable columns
    const extendedColumns: Column<TestItem>[] = [
      ...columns,
      {
        key: "id",
        header: "ID",
        render: (row) => <span>{row.id}</span>,
        sortable: true,
      },
    ];
    render(
      <DataTable
        columns={extendedColumns}
        data={data}
        keyFn={(row) => row.id}
      />
    );
    const scoreHeader = screen.getByText("Score");
    // Click score to sort desc
    fireEvent.click(scoreHeader);
    // Click again to toggle to asc
    fireEvent.click(scoreHeader.closest("th")!);
    const scoreTh = scoreHeader.closest("th");
    expect(scoreTh?.textContent).toContain("\u2191");

    // Now click ID header - should reset to desc
    const idHeader = screen.getByText("ID");
    fireEvent.click(idHeader);
    const idTh = idHeader.closest("th");
    expect(idTh?.textContent).toContain("\u2193");
    // Score header should no longer have any sort indicator
    expect(scoreTh?.textContent).not.toContain("\u2191");
    expect(scoreTh?.textContent).not.toContain("\u2193");
  });

  test("non-sortable columns do not have cursor-pointer class", () => {
    render(
      <DataTable columns={columns} data={data} keyFn={(row) => row.id} />
    );
    const nameHeader = screen.getByText("Name");
    expect(nameHeader.className).not.toContain("cursor-pointer");
  });
});
