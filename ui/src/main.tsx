// IoT Edge's own interface: a shared dashboard for anyone with its link, and everything else behind
// sign-in, so the first never loads the second.

import { isPublicPage } from "@ferrum/client";

void (isPublicPage() ? import("./public") : import("./app"));
