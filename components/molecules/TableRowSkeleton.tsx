import React from 'react';
import { Skeleton } from '@/components/atoms/Skeleton';

interface TableRowSkeletonProps {
  columns?: number;
  className?: string;
}

export function TableRowSkeleton({
  columns = 4,
  className = '',
}: TableRowSkeletonProps): React.ReactNode {
  return (
    <tr className={`border-b border-border ${className}`}>
      {Array.from({ length: columns }).map((_, index) => (
        <td key={index} className="p-4">
          <Skeleton className="h-4 w-full" />
        </td>
      ))}
    </tr>
  );
}

export function TableSkeletonRows({
  rows = 5,
  columns = 4,
  className = '',
}: {
  rows?: number;
  columns?: number;
  className?: string;
}): React.ReactNode {
  return (
    <tbody>
      {Array.from({ length: rows }).map((_, index) => (
        <TableRowSkeleton key={index} columns={columns} className={className} />
      ))}
    </tbody>
  );
}
