import type { Availability, ViewSpec } from "../api/types";

export type NavigationGroupId = "workload" | "data" | "host" | "events";
export type NavigationDestinationId =
  | "activity"
  | "statements"
  | "plans"
  | "tables"
  | "indexes"
  | "vacuum"
  | "os"
  | "events";

interface DestinationDefinition {
  id: NavigationDestinationId;
  labelKey: string;
  viewCode: string;
}

interface GroupDefinition {
  id: NavigationGroupId;
  labelKey: string;
  destinations: readonly DestinationDefinition[];
}

export interface NavigationDestination extends DestinationDefinition {
  availability: Availability;
  catalogView: ViewSpec | null;
}

export interface NavigationGroup {
  id: NavigationGroupId;
  labelKey: string;
  destinations: NavigationDestination[];
}

const GROUP_DEFINITIONS: readonly GroupDefinition[] = [
  {
    id: "workload",
    labelKey: "navigation.group.workload",
    destinations: [
      {
        id: "statements",
        labelKey: "tabs.statements",
        viewCode: "statements",
      },
      { id: "activity", labelKey: "tabs.activity", viewCode: "activity" },
      { id: "plans", labelKey: "tabs.plans", viewCode: "plans" },
    ],
  },
  {
    id: "data",
    labelKey: "navigation.group.data",
    destinations: [
      { id: "tables", labelKey: "tabs.tables", viewCode: "tables" },
      { id: "indexes", labelKey: "tabs.indexes", viewCode: "indexes" },
      { id: "vacuum", labelKey: "tabs.vacuum", viewCode: "vacuum" },
    ],
  },
  {
    id: "host",
    labelKey: "navigation.group.host",
    destinations: [
      {
        id: "os",
        labelKey: "navigation.destination.os",
        viewCode: "processes",
      },
    ],
  },
  {
    id: "events",
    labelKey: "navigation.group.events",
    destinations: [
      { id: "events", labelKey: "tabs.events", viewCode: "events" },
    ],
  },
] as const;

export function buildNavigationGroups(
  views: readonly ViewSpec[],
): NavigationGroup[] {
  const byCode = new Map(views.map((view) => [view.code, view]));
  return GROUP_DEFINITIONS.map((group) => ({
    id: group.id,
    labelKey: group.labelKey,
    destinations: group.destinations.map((definition) => {
      const catalogView = byCode.get(definition.viewCode) ?? null;
      return {
        ...definition,
        catalogView,
        availability: catalogView?.availability ?? "not_collected",
      };
    }),
  }));
}

export function availableDestinations(
  groups: readonly NavigationGroup[],
): NavigationDestination[] {
  return groups
    .flatMap((group) => group.destinations)
    .filter((destination) => destination.availability === "available");
}

export function destinationForView(
  groups: readonly NavigationGroup[],
  viewCode: string,
): NavigationDestination | null {
  return (
    groups
      .flatMap((group) => group.destinations)
      .find((destination) => destination.viewCode === viewCode) ?? null
  );
}
