import { describeError, log } from "../../log.js";
import { PlayerFailure, PlayerUnavailable, type PlayerHost } from "./host.js";

/** What the in-app player says it is doing right now. */
export interface SpeakerStatus {
  /** The player's own word: idle, buffering, playing, paused, ended or error. */
  status: string;
  position_ms: number;
  /** The last problem, in words a person can act on. */
  message?: string;
}

/**
 * The in-app player as a place Spotify can play to: the Connect device its window registered, and a
 * window onto what it is doing, so a play that Spotify accepted can be checked against what the
 * player then did. Nothing here plays anything.
 */
export interface Speaker {
  /** The Spotify device id of the in-app player, or null when there is none ready to use. */
  deviceId(): Promise<string | null>;
  /** What the player is doing, or null when it cannot be asked (no window, or it did not answer). */
  status(): Promise<SpeakerStatus | null>;
  /** Why the player cannot be used, in its own words, or null when it is usable or cannot say. */
  unavailableBecause(): Promise<string | null>;
}

interface DeviceReply {
  device_id?: string | null;
  ready?: boolean;
}

interface StateReply {
  status?: string;
  position_ms?: number;
  need?: string;
  ready?: boolean;
  message?: string;
}

/** Asks the player window, through the host. */
export class HostSpeaker implements Speaker {
  constructor(private readonly host: PlayerHost) {}

  /** The window's reply to `op`, or null when there is no window, it would not answer, or it said no. */
  private async ask<T>(op: string, timeoutMs: number): Promise<T | null> {
    try {
      // A short wait: this is a lookup in a window that is either there or not, on the same machine.
      return await this.host.call<T>(op, {}, timeoutMs);
    } catch (error) {
      if (!(error instanceof PlayerUnavailable) && !(error instanceof PlayerFailure)) throw error;
      // Not being able to find the speaker is the ordinary case, not a failure: say so quietly.
      log.debug("speaker_unavailable", "no in-app Spotify player to ask", {
        op,
        error: describeError(error),
      });
      return null;
    }
  }

  async deviceId(): Promise<string | null> {
    const reply = await this.ask<DeviceReply>("device", 4_000);
    return reply && reply.ready === true && typeof reply.device_id === "string" && reply.device_id !== ""
      ? reply.device_id
      : null;
  }

  async status(): Promise<SpeakerStatus | null> {
    const s = await this.ask<StateReply>("state", 3_000);
    if (!s || typeof s.status !== "string") return null;
    return {
      status: s.status,
      position_ms: typeof s.position_ms === "number" ? s.position_ms : 0,
      ...(typeof s.message === "string" && s.message !== "" ? { message: s.message } : {}),
    };
  }

  async unavailableBecause(): Promise<string | null> {
    const s = await this.ask<StateReply>("state", 3_000);
    if (!s || s.ready === true) return null;
    return typeof s.message === "string" && s.message !== "" ? s.message : null;
  }
}
