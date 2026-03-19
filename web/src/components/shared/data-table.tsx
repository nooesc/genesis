import * as React from 'react'
import {
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  getSortedRowModel,
  getPaginationRowModel,
  type SortingState,
  useReactTable,
} from '@tanstack/react-table'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'

const PAGE_SIZES = [25, 50, 100] as const

interface DataTableProps<TData, TValue> {
  columns: ColumnDef<TData, TValue>[]
  data: TData[]
  onRowClick?: (row: TData) => void
  meta?: Record<string, unknown>
  pageSize?: number
}

export function DataTable<TData, TValue>({
  columns,
  data,
  onRowClick,
  meta,
  pageSize = 25,
}: DataTableProps<TData, TValue>) {
  const [sorting, setSorting] = React.useState<SortingState>([])

  const table = useReactTable({
    data,
    columns,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    onSortingChange: setSorting,
    state: { sorting },
    initialState: { pagination: { pageSize } },
    meta,
  })

  const currentPage = table.getState().pagination.pageIndex + 1
  const totalPages = table.getPageCount()
  const currentPageSize = table.getState().pagination.pageSize

  return (
    <div className="flex flex-col gap-2">
      <div className="rounded-md border border-border/30">
        <Table>
          <TableHeader>
            {table.getHeaderGroups().map((headerGroup) => (
              <TableRow key={headerGroup.id} className="hover:bg-transparent border-border/20">
                {headerGroup.headers.map((header) => (
                  <TableHead
                    key={header.id}
                    className="font-mono text-[9px] uppercase tracking-wider text-muted-foreground/50"
                    onClick={header.column.getCanSort() ? header.column.getToggleSortingHandler() : undefined}
                    style={{ cursor: header.column.getCanSort() ? 'pointer' : 'default' }}
                  >
                    {header.isPlaceholder
                      ? null
                      : flexRender(header.column.columnDef.header, header.getContext())}
                    {header.column.getIsSorted() === 'asc' && ' ↑'}
                    {header.column.getIsSorted() === 'desc' && ' ↓'}
                  </TableHead>
                ))}
              </TableRow>
            ))}
          </TableHeader>
          <TableBody>
            {table.getRowModel().rows.length ? (
              table.getRowModel().rows.map((row) => (
                <TableRow
                  key={row.id}
                  onClick={() => onRowClick?.(row.original)}
                  className={`border-border/10 ${onRowClick ? 'cursor-pointer' : ''} hover:bg-card/40`}
                >
                  {row.getVisibleCells().map((cell) => (
                    <TableCell key={cell.id} className="py-2 font-mono text-xs">
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </TableCell>
                  ))}
                </TableRow>
              ))
            ) : (
              <TableRow>
                <TableCell
                  colSpan={columns.length}
                  className="h-24 text-center font-mono text-[10px] text-muted-foreground/40"
                >
                  No results.
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </div>

      {(totalPages > 1 || data.length > PAGE_SIZES[0]) && (
        <div className="flex items-center justify-between px-0.5">
          <div className="flex items-center gap-2">
            <span className="font-mono text-[9px] text-muted-foreground/30">
              {data.length} total
            </span>
            <div className="flex items-center gap-1">
              {PAGE_SIZES.map((size) => (
                <button
                  key={size}
                  onClick={() => table.setPageSize(size)}
                  className={`rounded px-1.5 py-0.5 font-mono text-[8px] transition-colors ${
                    currentPageSize === size
                      ? 'bg-primary/10 text-primary'
                      : 'text-muted-foreground/30 hover:text-muted-foreground'
                  }`}
                >
                  {size}
                </button>
              ))}
            </div>
          </div>
          {totalPages > 1 && (
            <div className="flex items-center gap-2">
              <span className="font-mono text-[9px] tabular-nums text-muted-foreground/30">
                {currentPage}/{totalPages}
              </span>
              <div className="flex items-center gap-1">
                <button
                  onClick={() => table.setPageIndex(0)}
                  disabled={!table.getCanPreviousPage()}
                  className="rounded px-1.5 py-0.5 font-mono text-[9px] text-muted-foreground/40 transition-colors hover:text-muted-foreground disabled:opacity-20"
                >
                  ««
                </button>
                <button
                  onClick={() => table.previousPage()}
                  disabled={!table.getCanPreviousPage()}
                  className="rounded px-1.5 py-0.5 font-mono text-[9px] text-muted-foreground/40 transition-colors hover:text-muted-foreground disabled:opacity-20"
                >
                  «
                </button>
                <button
                  onClick={() => table.nextPage()}
                  disabled={!table.getCanNextPage()}
                  className="rounded px-1.5 py-0.5 font-mono text-[9px] text-muted-foreground/40 transition-colors hover:text-muted-foreground disabled:opacity-20"
                >
                  »
                </button>
                <button
                  onClick={() => table.setPageIndex(totalPages - 1)}
                  disabled={!table.getCanNextPage()}
                  className="rounded px-1.5 py-0.5 font-mono text-[9px] text-muted-foreground/40 transition-colors hover:text-muted-foreground disabled:opacity-20"
                >
                  »»
                </button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  )
}
