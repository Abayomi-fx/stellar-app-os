'use client';

import React from 'react';
import { ProjectCardSkeleton } from '@/components/molecules/ProjectCard/ProjectCardSkeleton';
import { TableSkeletonRows } from '@/components/molecules/TableRowSkeleton';
import { DashboardStatGridSkeleton } from '@/components/molecules/DashboardStatSkeleton';
import { SkeletonText } from '@/components/atoms/Skeleton';

/**
 * ProjectCardGridSkeleton
 *
 * Shows multiple project card skeletons in a grid layout
 * Useful for: Marketplace, Blog, Project listings
 */
export function ProjectCardGridSkeleton({
  count = 6,
  className = '',
}: {
  count?: number;
  className?: string;
}): React.ReactNode {
  return (
    <div className={`grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-6 ${className}`}>
      {Array.from({ length: count }).map((_, i) => (
        <ProjectCardSkeleton key={i} />
      ))}
    </div>
  );
}

/**
 * DataTableSkeleton
 *
 * Shows table header and row skeletons
 * Useful for: Data tables, Admin pages, Listing pages
 */
export function DataTableSkeleton({
  rows = 5,
  columns = 5,
  className = '',
  showHeader = true,
}: {
  rows?: number;
  columns?: number;
  className?: string;
  showHeader?: boolean;
}): React.ReactNode {
  return (
    <div className={`overflow-x-auto ${className}`}>
      <table className="w-full border-collapse">
        {showHeader && (
          <thead>
            <tr className="border-b border-border">
              {Array.from({ length: columns }).map((_, i) => (
                <th key={i} className="text-left p-4 bg-muted/50">
                  <SkeletonText lines={1} className="w-24" animate={false} />
                </th>
              ))}
            </tr>
          </thead>
        )}
        <TableSkeletonRows rows={rows} columns={columns} />
      </table>
    </div>
  );
}

/**
 * HeroSectionSkeleton
 *
 * Shows hero section with image and text skeletons
 * Useful for: Page headers, Featured content
 */
export function HeroSectionSkeleton({ className = '' }: { className?: string }): React.ReactNode {
  return (
    <div className={`space-y-4 ${className}`}>
      {/* Image skeleton */}
      <div className="h-96 w-full bg-muted skeleton-shimmer rounded-lg" />

      {/* Text skeleton */}
      <div className="space-y-3 max-w-2xl">
        <div className="h-10 w-3/4 bg-muted skeleton-shimmer rounded" />
        <SkeletonText lines={3} />
      </div>
    </div>
  );
}

/**
 * CardGridSkeleton
 *
 * Generic grid of card-like skeletons
 * Useful for: Generic card layouts
 */
export function CardGridSkeleton({
  count = 4,
  cols = 'grid-cols-1 md:grid-cols-2 lg:grid-cols-4',
  className = '',
}: {
  count?: number;
  cols?: string;
  className?: string;
}): React.ReactNode {
  return (
    <div className={`grid ${cols} gap-4 ${className}`}>
      {Array.from({ length: count }).map((_, i) => (
        <div key={i} className="p-4 bg-card rounded-lg border border-border space-y-3">
          <div className="h-32 w-full bg-muted skeleton-shimmer rounded" />
          <SkeletonText lines={2} />
          <div className="h-10 w-full bg-muted skeleton-shimmer rounded" />
        </div>
      ))}
    </div>
  );
}

/**
 * ListSkeleton
 *
 * Shows list of items with skeletons
 * Useful for: Search results, Comments, Feed items
 */
export function ListSkeleton({
  count = 5,
  className = '',
}: {
  count?: number;
  className?: string;
}): React.ReactNode {
  return (
    <div className={`space-y-3 ${className}`}>
      {Array.from({ length: count }).map((_, i) => (
        <div key={i} className="p-4 bg-card rounded-lg border border-border space-y-2">
          <div className="flex items-start justify-between">
            <SkeletonText lines={1} className="w-1/2" />
            <div className="h-4 w-20 bg-muted skeleton-shimmer rounded" />
          </div>
          <SkeletonText lines={2} className="w-5/6" />
        </div>
      ))}
    </div>
  );
}

/**
 * ContentSkeleton
 *
 * Shows full page content skeleton with header and body
 * Useful for: Article pages, Detail pages
 */
export function ContentSkeleton({ className = '' }: { className?: string }): React.ReactNode {
  return (
    <article className={`max-w-3xl mx-auto ${className}`}>
      {/* Header */}
      <header className="mb-8 space-y-4">
        <div className="h-12 w-3/4 bg-muted skeleton-shimmer rounded" />
        <div className="flex gap-4">
          <div className="h-4 w-24 bg-muted skeleton-shimmer rounded" />
          <div className="h-4 w-32 bg-muted skeleton-shimmer rounded" />
        </div>
      </header>

      {/* Image */}
      <div className="h-96 w-full bg-muted skeleton-shimmer rounded-lg mb-8" />

      {/* Body content */}
      <div className="space-y-4">
        <SkeletonText lines={4} />
        <div className="h-48 w-full bg-muted skeleton-shimmer rounded" />
        <SkeletonText lines={3} />
      </div>
    </article>
  );
}

/**
 * DashboardSkeleton
 *
 * Shows dashboard layout with stat cards and charts
 * Useful for: Dashboard pages
 */
export function DashboardSkeleton({ className = '' }: { className?: string }): React.ReactNode {
  return (
    <div className={`space-y-8 ${className}`}>
      {/* Stats grid */}
      <DashboardStatGridSkeleton count={4} />

      {/* Chart area */}
      <div className="space-y-4">
        <div className="h-6 w-48 bg-muted skeleton-shimmer rounded" />
        <div className="h-64 w-full bg-muted skeleton-shimmer rounded-lg" />
      </div>

      {/* Table */}
      <div className="space-y-4">
        <div className="h-6 w-32 bg-muted skeleton-shimmer rounded" />
        <DataTableSkeleton rows={6} columns={5} />
      </div>
    </div>
  );
}
