import { useEffect, useRef, useState } from "react";

interface PanelResizeHandleProps {
  axis: "horizontal" | "vertical";
  label: string;
  maximum: number;
  minimum: number;
  onChange: (value: number) => void;
  value: number;
  reverse?: boolean;
}

export function PanelResizeHandle({ axis, label, maximum, minimum, onChange, reverse = false, value }: PanelResizeHandleProps) {
  const dragStart = useRef<{ coordinate: number; value: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  useEffect(() => {
    if (!dragging) return;
    const onPointerMove = (event: PointerEvent) => {
      const start = dragStart.current;
      if (!start) return;
      const coordinate = axis === "horizontal" ? event.clientX : event.clientY;
      const delta = (coordinate - start.coordinate) * (reverse ? -1 : 1);
      onChange(Math.min(maximum, Math.max(minimum, start.value + delta)));
    };
    const stop = () => {
      dragStart.current = null;
      setDragging(false);
    };
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", stop, { once: true });
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", stop);
    };
  }, [axis, dragging, maximum, minimum, onChange, reverse]);

  return (
    <div
      aria-label={label}
      aria-orientation={axis === "horizontal" ? "vertical" : "horizontal"}
      aria-valuemax={maximum}
      aria-valuemin={minimum}
      aria-valuenow={Math.round(value)}
      className={`panel-resize-handle panel-resize-handle--${axis}`}
      onDoubleClick={() => onChange(axis === "horizontal" ? 260 : 220)}
      onKeyDown={(event) => {
        const decreaseKey = axis === "horizontal" ? "ArrowLeft" : "ArrowUp";
        const increaseKey = axis === "horizontal" ? "ArrowRight" : "ArrowDown";
        if (event.key !== decreaseKey && event.key !== increaseKey) return;
        event.preventDefault();
        const direction = event.key === increaseKey ? 1 : -1;
        const delta = direction * (reverse ? -10 : 10);
        onChange(Math.min(maximum, Math.max(minimum, value + delta)));
      }}
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        dragStart.current = {
          coordinate: axis === "horizontal" ? event.clientX : event.clientY,
          value,
        };
        setDragging(true);
      }}
      role="separator"
      tabIndex={0}
    />
  );
}
