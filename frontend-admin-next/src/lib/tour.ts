"use client";

const KEY = "sag.tour.dismissed";

export function isTourDismissed(): boolean {
  if (typeof window === "undefined") return true;
  return window.localStorage.getItem(KEY) === "1";
}

export function dismissTour() {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(KEY, "1");
}
