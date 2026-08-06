import { useEffect, useState } from "react";

export interface ResponsivePanels {
  inspectorDrawer: boolean;
  sidebarDrawer: boolean;
}

const SIDEBAR_QUERY = "(max-width: 979px)";
const INSPECTOR_QUERY = "(max-width: 1099px)";

export function useResponsivePanels(override?: ResponsivePanels): ResponsivePanels {
  const [responsive, setResponsive] = useState<ResponsivePanels>(() => override ?? {
    inspectorDrawer: matchMedia(INSPECTOR_QUERY).matches,
    sidebarDrawer: matchMedia(SIDEBAR_QUERY).matches,
  });

  useEffect(() => {
    if (override) {
      setResponsive(override);
      return;
    }

    const sidebar = matchMedia(SIDEBAR_QUERY);
    const inspector = matchMedia(INSPECTOR_QUERY);
    const update = () => setResponsive({
      inspectorDrawer: inspector.matches,
      sidebarDrawer: sidebar.matches,
    });
    sidebar.addEventListener("change", update);
    inspector.addEventListener("change", update);
    update();
    return () => {
      sidebar.removeEventListener("change", update);
      inspector.removeEventListener("change", update);
    };
  }, [override]);

  return responsive;
}
