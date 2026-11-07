// IoT Edge's own interface: a shared dashboard for anyone with its link, and everything else behind
// sign-in, so the first never loads the second.

import { addCatalogs, isPublicPage } from "@ferrum/client";

// Its own words in the page's language (src/locales), fetched as the page starts; it is drawn once they are in.
void addCatalogs({ ar: () => import("./locales/ar.json"), hi: () => import("./locales/hi.json") });

void (isPublicPage() ? import("./public") : import("./app"));
