import { useState, useEffect, useRef, useCallback } from "react";
import { Clock } from "lucide-react";
import type { Schedule } from "../api/types";
import {
  cronToSlots,
  frequencyLabel,
  type ScheduleSlot,
} from "./calendarUtils";

interface Props {
  schedules: Schedule[];
}

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
// Day indices: Mon=1 Tue=2 Wed=3 Thu=4 Fri=5 Sat=6 Sun=0 (JS getDay())
const DAY_JS_INDEX = [1, 2, 3, 4, 5, 6, 0];

const HOURS = Array.from({ length: 24 }, (_, i) => i);

interface PopoverState {
  slot: ScheduleSlot;
  x: number;
  y: number;
}

export function ScheduleCalendar({ schedules }: Props) {
  const [popover, setPopover] = useState<PopoverState | null>(null);
  const [nowHour, setNowHour] = useState(() => new Date().getHours());
  const [nowMinute, setNowMinute] = useState(() => new Date().getMinutes());
  const [todayDow, setTodayDow] = useState(() => new Date().getDay());
  const containerRef = useRef<HTMLDivElement>(null);

  // Tick every minute to keep the current-time marker accurate.
  useEffect(() => {
    const tick = () => {
      const now = new Date();
      setNowHour(now.getHours());
      setNowMinute(now.getMinutes());
      setTodayDow(now.getDay());
    };
    // Align to the next minute boundary.
    const msToNextMinute = (60 - new Date().getSeconds()) * 1000;
    const timeout = setTimeout(() => {
      tick();
      const interval = setInterval(tick, 60_000);
      return () => clearInterval(interval);
    }, msToNextMinute);
    return () => clearTimeout(timeout);
  }, []);

  // Close popover when clicking outside.
  useEffect(() => {
    if (!popover) return;
    const handler = (e: MouseEvent) => {
      const target = e.target as Element;
      if (!target.closest(".sched-cal__popover") && !target.closest(".sched-cal__pill") && !target.closest(".sched-cal__dot")) {
        setPopover(null);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [popover]);

  // Scroll the body so the current hour is roughly centered on mount.
  const bodyRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!bodyRef.current) return;
    const ROW_H = 48;
    const visibleRows = Math.floor(520 / ROW_H);
    const targetRow = Math.max(0, nowHour - Math.floor(visibleRows / 2));
    bodyRef.current.scrollTop = targetRow * ROW_H;
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Build slots from schedules.
  const slots: ScheduleSlot[] = schedules.map((s, i) => cronToSlots(s, i));

  // Group slots by hour × col (col = 0-6, Mon–Sun).
  type CellKey = `${number}-${number}`;
  const cellSlots = new Map<CellKey, ScheduleSlot[]>();

  for (const slot of slots) {
    if (slot.frequency === "hourly") {
      // Show a dot in every hour × every column.
      for (let h = 0; h < 24; h++) {
        for (let col = 0; col < 7; col++) {
          const key: CellKey = `${h}-${col}`;
          const list = cellSlots.get(key) ?? [];
          list.push(slot);
          cellSlots.set(key, list);
        }
      }
    } else if (slot.frequency === "weekly" && slot.daysOfWeek.length > 0) {
      // Only columns matching the DOW.
      for (const dow of slot.daysOfWeek) {
        const col = DAY_JS_INDEX.indexOf(dow);
        if (col === -1) continue;
        const key: CellKey = `${slot.hour}-${col}`;
        const list = cellSlots.get(key) ?? [];
        list.push(slot);
        cellSlots.set(key, list);
      }
    } else {
      // daily / monthly / custom — show in all columns.
      for (let col = 0; col < 7; col++) {
        const key: CellKey = `${slot.hour}-${col}`;
        const list = cellSlots.get(key) ?? [];
        list.push(slot);
        cellSlots.set(key, list);
      }
    }
  }

  const handlePillClick = useCallback(
    (e: React.MouseEvent, slot: ScheduleSlot) => {
      e.stopPropagation();
      const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
      setPopover({
        slot,
        x: Math.min(rect.right + 8, window.innerWidth - 296),
        y: Math.min(rect.top, window.innerHeight - 180),
      });
    },
    [],
  );

  // Pixel offset of the current-time marker within a cell.
  const nowOffsetPx = (nowMinute / 60) * 48;

  return (
    <div ref={containerRef} style={{ position: "relative" }}>
      <div className="sched-cal">
        {/* ── Header row ─────────────────────────────────── */}
        <div className="sched-cal__header-spacer" />
        {DAYS.map((day, col) => {
          const jsDay = DAY_JS_INDEX[col];
          return (
            <div
              key={day}
              className={`sched-cal__day-header${jsDay === todayDow ? " is-today" : ""}`}
            >
              {day}
            </div>
          );
        })}

        {/* ── Scrollable body ─────────────────────────────── */}
        <div className="sched-cal__body" ref={bodyRef}>
          {HOURS.map((h) => (
            <div key={h} className="sched-cal__hour-row">
              {/* Time label */}
              <div className="sched-cal__time-label">
                {String(h).padStart(2, "0")}:00
                {/* Current-time marker on the time label column */}
                {h === nowHour && (
                  <div
                    className="sched-cal__time-marker"
                    style={{ top: nowOffsetPx }}
                  />
                )}
              </div>

              {/* 7 day cells */}
              {DAYS.map((_, col) => {
                const key: CellKey = `${h}-${col}`;
                const cellItems = cellSlots.get(key) ?? [];
                const jsDay = DAY_JS_INDEX[col];
                const isToday = jsDay === todayDow;

                return (
                  <div
                    key={col}
                    className={`sched-cal__cell${isToday ? " is-today" : ""}`}
                  >
                    {/* Current-time marker */}
                    {h === nowHour && isToday && (
                      <div
                        className="sched-cal__time-marker"
                        style={{ top: nowOffsetPx }}
                      />
                    )}

                    {/* Pills / dots for this cell */}
                    <div style={{ display: "flex", flexWrap: "wrap", gap: 2, alignItems: "flex-start" }}>
                      {cellItems.map((slot) =>
                        slot.frequency === "hourly" ? (
                          <button
                            key={slot.scheduleId}
                            className="sched-cal__dot"
                            style={{ background: slot.color }}
                            title={slot.label}
                            onClick={(e) => handlePillClick(e, slot)}
                            aria-label={slot.label}
                          />
                        ) : (
                          <button
                            key={slot.scheduleId}
                            className="sched-cal__pill"
                            style={{ background: slot.color }}
                            onClick={(e) => handlePillClick(e, slot)}
                            aria-label={slot.label}
                          >
                            <Clock size={8} />
                            {slot.label}
                          </button>
                        )
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </div>

      {/* ── Pill popover ─────────────────────────────────── */}
      {popover && (
        <div
          className="sched-cal__popover"
          style={{ top: popover.y, left: popover.x }}
          role="tooltip"
        >
          <div className="sched-cal__popover-title">{popover.slot.label}</div>
          <code className="sched-cal__popover-cron">
            {schedules.find((s) => s.id === popover.slot.scheduleId)?.cron ?? ""}
          </code>
          <div className="sched-cal__popover-freq">
            {frequencyLabel(popover.slot)}
          </div>
          {(() => {
            const s = schedules.find((sc) => sc.id === popover.slot.scheduleId);
            if (!s?.prompt) return null;
            return (
              <div className="sched-cal__popover-result">
                {s.prompt.slice(0, 120)}
                {s.prompt.length > 120 ? "..." : ""}
              </div>
            );
          })()}
        </div>
      )}
    </div>
  );
}
