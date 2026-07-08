import { useAppDispatch } from "../state/AppContext";
import { NotificationsView } from "../hub/views/Notifications";
import type { GuiSection } from "../desktopState";
import type { ScheduleRunNotification } from "../api/types";

export function Notifications() {
  const dispatch = useAppDispatch();

  function go(route: string, run?: ScheduleRunNotification) {
    if (route === "canvas" && run) {
      dispatch({ type: "SET_DEBRIEF_CONTEXT", payload: { type: "debrief", run } });
    }
    dispatch({ type: "SET_SECTION", payload: route as GuiSection });
  }

  return (
    <div className="screen screen--notifications">
      <NotificationsView go={go} />
    </div>
  );
}
