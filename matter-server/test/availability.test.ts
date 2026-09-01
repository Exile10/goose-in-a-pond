import { describe, expect, it } from "vitest";
import type { ClientNode } from "@matter/main";

import { availabilityReports } from "../src/controller.js";

/**
 * The two fields `availabilityReports` reads. A real `ClientNode` needs a fabric and
 * a network, which is the reason this function was pulled out of the class.
 */
function peer(nodeId: number | undefined, online: boolean): ClientNode {
  return {
    peerAddress: nodeId === undefined ? undefined : { nodeId },
    lifecycle: { isOnline: online },
  } as unknown as ClientNode;
}

describe("availability reports", () => {
  it("names every peer, whether or not its reachability changed", () => {
    // The bug this exists for. `device_availability` was an EDGE: it was sent only
    // from matter.js's `lifecycle.online` / `.offline`, which fire on a transition --
    // and, per `#retryWiring`'s own comment, a node already online when the
    // controller connects never fires `online` again. So the bridge's set of devices
    // it vouches for was seeded once from a `subscribe` snapshot reading
    // `peer.lifecycle.isOnline`, which is false until a CASE session exists. A
    // snapshot taken inside that window recorded a working device as offline, nothing
    // ever said otherwise, `last_seen` aged past five minutes, and the card went
    // offline while readings kept arriving from the controller's own cache.
    //
    // Reporting the LEVEL every tick is what makes that recoverable, and this is the
    // property: the report is unconditional.
    const reports = availabilityReports([peer(2, true), peer(3, false)]);

    expect(reports).toEqual([
      { deviceId: "matter-2", online: true },
      { deviceId: "matter-3", online: false },
    ]);
  });

  it("repeats an unchanged report rather than falling silent", () => {
    // Two ticks over the same peers. An edge report says nothing the second time,
    // which is exactly the failure; a level report says the same thing again, and a
    // bridge that lost the first one recovers on the second.
    const peers = [peer(2, true)];

    expect(availabilityReports(peers)).toEqual(availabilityReports(peers));
    expect(availabilityReports(peers)).toHaveLength(1);
  });

  it("leaves out a node that has only been discovered, not commissioned", () => {
    // Discovery adds merely-commissionable nodes to the same collection. They are not
    // devices on this fabric, and reporting one as available would announce a device
    // GIAP does not have.
    expect(availabilityReports([peer(undefined, true), peer(4, true)])).toEqual([
      { deviceId: "matter-4", online: true },
    ]);
  });
});
