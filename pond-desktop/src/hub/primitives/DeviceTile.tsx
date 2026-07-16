import React from "react";
import { HubIco, lockEl, unlockEl, dotsEl } from "./HubIco";
import { HP_PATHS } from "./icons";
import { useDeviceState } from "../state/hubStore";
import type { DeviceData } from "../data/mockHome";

interface DeviceTileProps {
  device: DeviceData;
  size?: "md" | "lg";
}

export function DeviceTile({ device, size = "md" }: DeviceTileProps) {
  const [st, , control] = useDeviceState(device.id);
  const k = device.kind;
  const { on, locked, target = 70 } = st;

  // Resolve visual state
  let active = false;
  let bg = "var(--tile-bg,#fff)";
  let fg = "var(--tile-fg,#18181B)";
  let sub = "var(--tile-sub,#94A3B8)";
  let iconEl: string | React.ReactNode;
  let statusText = "";
  let accentIcon = "var(--tile-icon,#CBD5E1)";
  const border = active ? undefined : "1px solid var(--tile-border,#ECECF1)";

  if (k === "light") {
    active = on;
    if (on) {
      bg = "linear-gradient(150deg,#FBBF24,#F59E0B)";
      fg = "#fff";
      sub = "rgba(255,255,255,.9)";
      accentIcon = "#fff";
    }
    iconEl = on ? HP_PATHS.bulb : HP_PATHS.bulbOff;
    statusText = on ? `On · ${st.brightness}%` : "Off";
  } else if (k === "lock") {
    active = locked;
    if (locked) {
      bg = "linear-gradient(150deg,#3B82F6,#2563EB)";
      fg = "#fff";
      sub = "rgba(255,255,255,.9)";
      accentIcon = "#fff";
    }
    iconEl = locked ? lockEl : unlockEl;
    statusText = locked ? "Locked" : "Unlocked";
  } else if (k === "thermo") {
    active = true;
    bg = "linear-gradient(150deg,#FB923C,#EA580C)";
    fg = "#fff";
    sub = "rgba(255,255,255,.92)";
    accentIcon = "#fff";
    iconEl = HP_PATHS.flame;
    statusText = `${st.mode} to ${target}°`;
  } else if (k === "plug") {
    active = on;
    if (on) {
      bg = "linear-gradient(150deg,#2DD4BF,#0D9488)";
      fg = "#fff";
      sub = "rgba(255,255,255,.9)";
      accentIcon = "#fff";
    }
    iconEl = HP_PATHS.plug;
    statusText = on ? `On · ${st.watts}W` : "Off";
  }

  function handle() {
    if (k === "light" || k === "plug") void control({ on: !on });
    else if (k === "lock") void control({ locked: !locked });
    else if (k === "thermo") void control({ target: target >= 74 ? 66 : target + 1 });
  }

  function openCtrl() {
    window.dispatchEvent(new CustomEvent("hub:device", { detail: device.id }));
  }

  return (
    // A "..." controls button can't be nested inside the tile's own button
    // (nested interactive controls are invalid a11y-wise). They're siblings
    // instead: the tile is the primary toggle, the dots button is absolutely
    // positioned over its corner via .dtile-wrap in hub.css.
    <div className="dtile-wrap">
      <button
        className="dtile"
        onClick={handle}
        data-active={active}
        style={{
          background: bg,
          color: fg,
          ...(active ? {} : { border }),
        }}
      >
        <div className="dtile__top">
          <span className="dtile__name" style={{ color: fg }}>{device.name}</span>
        </div>
        {k === "thermo" ? (
          <div className="dtile__temp">
            {st.cur ?? device.value}<span>°</span>
          </div>
        ) : <div style={{ flex: 1 }} />}
        <div className="dtile__bottom">
          <span
            className="dtile__icon"
            style={{ background: active ? "rgba(255,255,255,.22)" : "var(--tile-iconbg,#F4F4F7)" }}
          >
            <HubIco d={iconEl ?? ""} size={size === "lg" ? 20 : 18} color={accentIcon} sw={2} />
          </span>
          <span className="dtile__status" style={{ color: sub }}>{statusText}</span>
        </div>
      </button>
      <button
        className="dtile__dots"
        onClick={openCtrl}
        aria-label={`${device.name} controls`}
        style={{ color: active ? "rgba(255,255,255,.7)" : "#C4C4CC" }}
      >
        <HubIco d={dotsEl} size={16} />
      </button>
    </div>
  );
}
