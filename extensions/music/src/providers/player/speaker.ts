import { describeError, log } from "../../log.js";
import { PlayerFailure, PlayerUnavailable, type PlayerHost } from "./host.js";

/**
 * The in-app player as a place Spotify can play to: the Connect device its window registered.
 * Nothing here plays anything; it only says whether there is a device to address, and which.
 */
export interface Speaker {
  /** The Spotify device id of the in-app player, or null when there is none ready to use. */
  deviceId(): Promise<string | null>;
}

interface DeviceReply {
  device_id?: string | null;
  ready?: boolean;
}

/** Asks the player window, through the host, for the device it registered. */
export class HostSpeaker implements Speaker {
  constructor(private readonly host: PlayerHost) {}

  async deviceId(): Promise<string | null> {
    try {
      // A short wait: this is a lookup in a window that is either there or not, on the same machine.
      const reply = await this.host.call<DeviceReply>("device", {}, 4_000);
      return reply.ready === true && typeof reply.device_id === "string" && reply.device_id !== ""
        ? reply.device_id
        : null;
    } catch (error) {
      if (!(error instanceof PlayerUnavailable) && !(error instanceof PlayerFailure)) throw error;
      // Not being able to find the speaker is the ordinary case, not a failure: say so quietly.
      log.debug("speaker_unavailable", "no in-app Spotify player to play on", {
        error: describeError(error),
      });
      return null;
    }
  }
}
