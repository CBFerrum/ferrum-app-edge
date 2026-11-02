// IoT Edge's own interface (Ferrum docs/plan/edge.md), built with Ferrum-Frontend's packages and libraries
// from the sibling checkout at ../../ferrum/web.
import { ferrumApp } from "../../ferrum/web/packages/build/vite.mjs";

export default ferrumApp(import.meta.url);
