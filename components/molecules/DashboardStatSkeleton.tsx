import React from 'react';
import { Skeleton, SkeletonText } from '@/components/atoms/Skeleton';
import { Card, CardContent, CardHeader } from '@/components/molecules/Card';

interface DashboardStatSkeletonProps {
  className?: string;
  showChart?: boolean;
}

export function DashboardStatSkeleton({
  className = '',
  showChart = false,
}: DashboardStatSkeletonProps): React.ReactNode {
  return (
    <Card className={`flex flex-col h-full ${className}`}>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between">
          <Skeleton className="h-5 w-32" />
          <Skeleton variant="circle" className="h-8 w-8" />
        </div>
      </CardHeader>

      <CardContent className="p-5 grow flex flex-col gap-4">
        {/* Main Value */}
        <div>
          <Skeleton className="h-8 w-24 mb-2" />
          <Skeleton className="h-4 w-40" />
        </div>

        {/* Change Indicator */}
        <div className="flex items-center gap-2">
          <Skeleton variant="circle" className="h-5 w-5" />
          <Skeleton className="h-4 w-20" />
        </div>

        {/* Chart or Details */}
        {showChart && (
          <div className="mt-4 space-y-2">
            <div className="flex gap-1 items-end justify-center h-16">
              {Array.from({ length: 7 }).map((_, i) => (
                <div
                  key={i}
                  className="flex-1 rounded-t-sm bg-muted skeleton-shimmer"
                  style={{
                    height: `${20 + Math.random() * 60}%`,
                  }}
                />
              ))}
            </div>
          </div>
        )}

        {/* Details Text */}
        <SkeletonText lines={2} className="text-xs mt-auto" />
      </CardContent>
    </Card>
  );
}

interface DashboardStatGridSkeletonProps {
  count?: number;
  className?: string;
}

export function DashboardStatGridSkeleton({
  count = 4,
  className = '',
}: DashboardStatGridSkeletonProps): React.ReactNode {
  return (
    <div className={`grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4 ${className}`}>
      {Array.from({ length: count }).map((_, index) => (
        <DashboardStatSkeleton key={index} />
      ))}
    </div>
  );
}
