import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type FocusEvent,
  type ReactNode,
  type UIEvent,
} from "react";

const DEFAULT_THRESHOLD = 100;
const DEFAULT_VISIBLE_ROWS = 20;
const DEFAULT_OVERSCAN = 6;
const END_TOLERANCE_PX = 4;

interface WindowedListProps<T> {
  "aria-label": string;
  "aria-live"?: "assertive" | "off" | "polite";
  as: "ol" | "ul";
  className: string;
  estimatedRowHeight: number;
  followEnd?: boolean;
  itemKey: (item: T) => string;
  items: readonly T[];
  keyboardScrollable?: boolean;
  renderItem: (item: T, index: number) => ReactNode;
  threshold?: number;
}

interface VisibleRow<T> {
  index: number;
  item: T;
  key: string;
  top: number;
}

/**
 * Keeps large console/resource lists bounded without changing the small-list DOM.
 * Focused rows are pinned until focus leaves the list, so scrolling cannot
 * unmount a user's active control.
 */
export function WindowedList<T>({
  "aria-label": ariaLabel,
  "aria-live": ariaLive,
  as: Tag,
  className,
  estimatedRowHeight,
  followEnd = false,
  itemKey,
  items,
  keyboardScrollable = false,
  renderItem,
  threshold = DEFAULT_THRESHOLD,
}: WindowedListProps<T>) {
  const listRef = useRef<HTMLOListElement | HTMLUListElement>(null);
  const measuredHeights = useRef(new Map<string, number>());
  const shouldFollowEnd = useRef(true);
  const previousItemCount = useRef(0);
  const previousContentHeight = useRef(0);
  const previousVisibleHeight = useRef(0);
  const [focusedKey, setFocusedKey] = useState<string | null>(null);
  const [heightVersion, setHeightVersion] = useState(0);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(estimatedRowHeight * DEFAULT_VISIBLE_ROWS);
  const windowed = items.length > threshold;

  const layout = useMemo(() => {
    const offsets = new Array<number>(items.length + 1);
    offsets[0] = 0;
    for (let index = 0; index < items.length; index += 1) {
      const key = itemKey(items[index]);
      offsets[index + 1] = offsets[index] + (measuredHeights.current.get(key) ?? estimatedRowHeight);
    }
    return { offsets, totalHeight: offsets[offsets.length - 1] ?? 0, version: heightVersion };
  }, [estimatedRowHeight, heightVersion, itemKey, items]);

  const rows = useMemo<VisibleRow<T>[]>(() => {
    if (!windowed) {
      return items.map((item, index) => ({ index, item, key: itemKey(item), top: 0 }));
    }
    const firstVisible = indexAtOffset(layout.offsets, scrollTop);
    const lastVisible = indexAtOffset(layout.offsets, scrollTop + viewportHeight);
    const start = Math.max(0, firstVisible - DEFAULT_OVERSCAN);
    const end = Math.min(items.length, lastVisible + DEFAULT_OVERSCAN + 1);
    const indexes = new Set<number>();
    for (let index = start; index < end; index += 1) indexes.add(index);

    if (focusedKey !== null) {
      const focusedIndex = items.findIndex((item) => itemKey(item) === focusedKey);
      if (focusedIndex >= 0) indexes.add(focusedIndex);
    }

    return [...indexes]
      .sort((left, right) => left - right)
      .map((index) => ({
        index,
        item: items[index],
        key: itemKey(items[index]),
        top: layout.offsets[index],
      }));
  }, [focusedKey, itemKey, items, layout.offsets, scrollTop, viewportHeight, windowed]);

  useEffect(() => {
    const currentKeys = new Set(items.map(itemKey));
    for (const key of measuredHeights.current.keys()) {
      if (!currentKeys.has(key)) measuredHeights.current.delete(key);
    }
    if (focusedKey !== null && !currentKeys.has(focusedKey)) setFocusedKey(null);
  }, [focusedKey, itemKey, items]);

  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list) return;
    const measured = list.querySelectorAll<HTMLElement>("[data-windowed-key]");
    let changed = false;
    for (const row of measured) {
      const key = row.dataset.windowedKey;
      const height = row.getBoundingClientRect().height;
      if (!key || height <= 0 || measuredHeights.current.get(key) === height) continue;
      measuredHeights.current.set(key, height);
      changed = true;
    }
    if (changed) setHeightVersion((version) => version + 1);
  }, [rows]);

  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list) return;
    const measuredViewport = list.clientHeight;
    if (measuredViewport > 0) setViewportHeight(measuredViewport);
  }, [windowed]);

  useEffect(() => {
    const list = listRef.current;
    if (!list || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (list.clientHeight > 0) setViewportHeight(list.clientHeight);
    });
    observer.observe(list);
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(() => {
    const previousCount = previousItemCount.current;
    previousItemCount.current = items.length;
    const list = listRef.current;
    if (!list) return;
    const contentHeight = Math.max(list.scrollHeight, layout.totalHeight);
    const contentHeightChanged = contentHeight !== previousContentHeight.current;
    previousContentHeight.current = contentHeight;
    const visibleHeight = list.clientHeight || viewportHeight;
    const visibleHeightChanged = visibleHeight !== previousVisibleHeight.current;
    previousVisibleHeight.current = visibleHeight;
    if (
      !followEnd
      || !shouldFollowEnd.current
      || (items.length <= previousCount && !contentHeightChanged && !visibleHeightChanged)
    ) return;
    const nextScrollTop = Math.max(0, contentHeight - visibleHeight);
    list.scrollTop = nextScrollTop;
    setScrollTop(nextScrollTop);
  }, [followEnd, items.length, layout.totalHeight, viewportHeight]);

  const handleScroll = (event: UIEvent<HTMLOListElement | HTMLUListElement>) => {
    const list = event.currentTarget;
    const contentHeight = Math.max(list.scrollHeight, layout.totalHeight);
    const visibleHeight = list.clientHeight || viewportHeight;
    shouldFollowEnd.current = contentHeight - visibleHeight - list.scrollTop <= END_TOLERANCE_PX;
    setScrollTop(list.scrollTop);
  };

  const handleFocus = (event: FocusEvent<HTMLOListElement | HTMLUListElement>) => {
    const row = (event.target as HTMLElement).closest<HTMLElement>("[data-windowed-key]");
    setFocusedKey(row?.dataset.windowedKey ?? null);
  };

  const handleBlur = (event: FocusEvent<HTMLOListElement | HTMLUListElement>) => {
    if (event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget)) return;
    setFocusedKey(null);
  };

  const listStyle = windowed
    ? ({ "--windowed-content-height": `${layout.totalHeight}px` } as CSSProperties)
    : undefined;

  return (
    <Tag
      aria-label={ariaLabel}
      aria-live={ariaLive}
      className={`${className}${windowed ? " windowed-list" : ""}`}
      onBlurCapture={handleBlur}
      onFocusCapture={handleFocus}
      onScroll={handleScroll}
      ref={listRef as never}
      style={listStyle}
      tabIndex={keyboardScrollable ? 0 : undefined}
    >
      {rows.map(({ index, item, key, top }) => (
        <li
          aria-posinset={windowed ? index + 1 : undefined}
          aria-setsize={windowed ? items.length : undefined}
          data-windowed-index={windowed ? index : undefined}
          data-windowed-key={windowed ? key : undefined}
          key={key}
          style={windowed ? { top } : undefined}
        >
          {renderItem(item, index)}
        </li>
      ))}
    </Tag>
  );
}

function indexAtOffset(offsets: readonly number[], target: number) {
  const itemCount = offsets.length - 1;
  if (itemCount <= 0) return 0;
  let low = 0;
  let high = itemCount;
  while (low < high) {
    const middle = Math.floor((low + high + 1) / 2);
    if (offsets[middle] <= target) low = middle;
    else high = middle - 1;
  }
  return Math.min(low, itemCount - 1);
}
