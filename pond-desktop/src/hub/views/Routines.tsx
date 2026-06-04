import React, { useState } from "react";
import "./routines.css";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { ROUTINES, type RoutineId } from "../data/routines";

// ─── RoutinesView ──────────────────────────────────────────────
// Phase 6: one-tap scene cards. These are on-demand macros, NOT
// time-triggered schedules. Future Phase 8 wires Run to api.executeRecipe().

export function RoutinesView() {
  const [running, setRunning] = useState<RoutineId | null>(null);

  const handleRun = (id: RoutineId) => {
    setRunning(id);
    // TODO (Phase 8): wire to api.executeRecipe(id) — AgentRecipe execution
    setTimeout(() => setRunning(null), 1600);
  };

  const handleNewRoutine = () => {
    // TODO (Phase 8): open recipe builder modal — creates an AgentRecipe
    console.info("TODO: open recipe builder modal");
  };

  const handleCreateCard = () => {
    // TODO (Phase 8): open recipe builder modal — creates an AgentRecipe
    console.info("TODO: open recipe builder modal");
  };

  return (
    <div className="rt">
      <header className="view-head">
        <div>
          <h1 className="view-title">Routines</h1>
          <p className="view-sub">
            One tap to set the whole house. Goose runs these for you.
          </p>
        </div>
        <button className="primary-btn" onClick={handleNewRoutine}>
          <HubIco d={HP_PATHS.plus} size={16} color="#fff" />
          + New routine
        </button>
      </header>

      <div className="rt__grid">
        {ROUTINES.map((routine) => {
          const isRunning = running === routine.id;
          return (
            <div key={routine.id} className="rt-card">
              {/* top: icon + name + time */}
              <div className="rt-card__top">
                <span
                  className="rt-card__icon"
                  style={{ background: routine.bg }}
                >
                  <HubIco d={routine.iconPath} size={22} color="#fff" sw={2} />
                </span>
                <div>
                  <div className="rt-card__name">{routine.name}</div>
                  <div className="rt-card__time">{routine.time}</div>
                </div>
              </div>

              {/* does chips */}
              <div className="rt-card__does">
                {routine.does.map((chip) => (
                  <span key={chip} className="rt-card__chip">
                    {chip}
                  </span>
                ))}
              </div>

              {/* run button */}
              <button
                className="rt-card__run"
                data-running={isRunning}
                style={
                  isRunning
                    ? {
                        background: routine.color,
                        borderColor: routine.color,
                        color: "#fff",
                      }
                    : { color: routine.color }
                }
                onClick={() => handleRun(routine.id)}
              >
                {isRunning ? (
                  <>
                    <HubIco d={HP_PATHS.check} size={15} color="#fff" sw={3} />
                    Running...
                  </>
                ) : (
                  <>
                    <HubIco
                      d={HP_PATHS.play}
                      size={14}
                      color={routine.color}
                      fill={routine.color}
                    />
                    Run now
                  </>
                )}
              </button>
            </div>
          );
        })}

        {/* dashed "Create a routine" placeholder tile */}
        <button className="rt-card rt-card--new" onClick={handleCreateCard}>
          <HubIco d={HP_PATHS.plus} size={26} color="#9A8FB8" />
          <span>Create a routine</span>
        </button>
      </div>
    </div>
  );
}
