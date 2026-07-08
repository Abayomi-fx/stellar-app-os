'use client';

import { useState, useEffect, useRef, useCallback } from 'react';
import type { TreeStatusEvent } from '@/lib/tree-status/types';

interface UseTreeStatusOptions {
  onEvent?: (event: TreeStatusEvent) => void;
}

interface UseTreeStatusReturn {
  events: TreeStatusEvent[];
  isConnected: boolean;
  error: Event | null;
}

export function useTreeStatus(options: UseTreeStatusOptions = {}): UseTreeStatusReturn {
  const [events, setEvents] = useState<TreeStatusEvent[]>([]);
  const [isConnected, setIsConnected] = useState(false);
  const [error, setError] = useState<Event | null>(null);
  const eventSourceRef = useRef<EventSource | null>(null);
  const onEventRef = useRef(options.onEvent);
<<<<<<< HEAD
  onEventRef.current = options.onEvent; // <-- ESLint hates this
  onEventRef.current = options.onEvent;
=======

  useEffect(() => {
    onEventRef.current = options.onEvent;
  }, [options.onEvent]);
>>>>>>> 4fa2ff0e46c01b84d0a39c3524e33dea37e50005

  useEffect(() => {
    const es = new EventSource('/api/trees/status');
    eventSourceRef.current = es;

    es.onopen = () => setIsConnected(true);

    es.addEventListener('tree-status', (e: MessageEvent) => {
      try {
        const data: TreeStatusEvent = JSON.parse(e.data);
        setEvents((prev) => [data, ...prev]);
        onEventRef.current?.(data);
      } catch {
        // ignore malformed events
      }
    });

    es.onerror = (err) => {
      setError(err);
      setIsConnected(false);
    };

    return () => {
      es.close();
      eventSourceRef.current = null;
    };
  }, []);

  const clearEvents = useCallback(() => setEvents([]), []);

  return { events, isConnected, error, clearEvents } as UseTreeStatusReturn & {
    clearEvents: () => void;
  };
}
