import React from 'react';
import { cn } from '@/lib/utils';

interface SkeletonProps {
  className?: string;
  variant?: 'default' | 'circle' | 'text';
  animate?: boolean;
}

export function Skeleton({
  className,
  variant = 'default',
  animate = true,
}: SkeletonProps): React.ReactNode {
  const baseClass = 'bg-muted';

  const variantClass = {
    default: 'rounded-md',
    circle: 'rounded-full',
    text: 'rounded h-4 w-full',
  }[variant];

  const animationClass = animate ? 'skeleton-shimmer' : '';

  return <div className={cn(baseClass, variantClass, animationClass, className)} />;
}

export function SkeletonText({
  lines = 1,
  className,
  animate = true,
}: {
  lines?: number;
  className?: string;
  animate?: boolean;
}): React.ReactNode {
  return (
    <div className={cn('space-y-2', className)}>
      {Array.from({ length: lines }).map((_, i) => (
        <Skeleton
          key={i}
          variant="text"
          animate={animate}
          className={i === lines - 1 ? 'w-5/6' : 'w-full'}
        />
      ))}
    </div>
  );
}
