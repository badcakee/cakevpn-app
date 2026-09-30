// Where the locations are on the home screen's map.

import { WORLD_SCALE, WORLD_TOP, WORLD_WIDTH } from "./worldmap";

/** The Natural Earth projection, as used for the outline in worldmap.ts: degrees to map units. */
export function project(lat: number, lon: number): [number, number] {
  const l = (lon * Math.PI) / 180;
  const p = (lat * Math.PI) / 180;
  const p2 = p * p;
  const p4 = p2 * p2;
  const x = l * (0.8707 - 0.131979 * p2 + p4 * (-0.013791 + p4 * (0.003971 * p2 - 0.001529 * p4)));
  const y = p * (1.007226 + p2 * (0.015085 + p4 * (-0.044475 + 0.028874 * p2 - 0.005916 * p4)));
  return [WORLD_WIDTH / 2 + x * WORLD_SCALE, (WORLD_TOP - y) * WORLD_SCALE];
}

/** Cities where servers are commonly hosted: latitude, longitude. */
const CITIES: Record<string, [number, number]> = {
  gravelines: [50.99, 2.13],
  roubaix: [50.69, 3.17],
  strasbourg: [48.57, 7.75],
  paris: [48.86, 2.35],
  marseille: [43.3, 5.37],
  frankfurt: [50.11, 8.68],
  falkenstein: [50.48, 12.37],
  nuremberg: [49.45, 11.08],
  berlin: [52.52, 13.4],
  dusseldorf: [51.23, 6.78],
  amsterdam: [52.37, 4.9],
  london: [51.51, -0.13],
  manchester: [53.48, -2.24],
  dublin: [53.35, -6.26],
  brussels: [50.85, 4.35],
  luxembourg: [49.61, 6.13],
  zurich: [47.38, 8.54],
  geneva: [46.2, 6.14],
  vienna: [48.21, 16.37],
  milan: [45.46, 9.19],
  rome: [41.9, 12.5],
  madrid: [40.42, -3.7],
  barcelona: [41.39, 2.17],
  lisbon: [38.72, -9.14],
  warsaw: [52.23, 21.01],
  prague: [50.08, 14.44],
  bucharest: [44.43, 26.1],
  sofia: [42.7, 23.32],
  helsinki: [60.17, 24.94],
  stockholm: [59.33, 18.07],
  oslo: [59.91, 10.75],
  copenhagen: [55.68, 12.57],
  istanbul: [41.01, 28.98],
  moscow: [55.76, 37.62],
  kyiv: [50.45, 30.52],
  dallas: [32.78, -96.8],
  ashburn: [39.04, -77.49],
  "new york": [40.71, -74.01],
  newark: [40.74, -74.17],
  chicago: [41.88, -87.63],
  atlanta: [33.75, -84.39],
  miami: [25.76, -80.19],
  phoenix: [33.45, -112.07],
  denver: [39.74, -104.99],
  "los angeles": [34.05, -118.24],
  "san jose": [37.34, -121.89],
  "san francisco": [37.77, -122.42],
  seattle: [47.61, -122.33],
  hillsboro: [45.52, -122.99],
  "las vegas": [36.17, -115.14],
  toronto: [43.65, -79.38],
  montreal: [45.5, -73.57],
  beauharnois: [45.31, -73.87],
  vancouver: [49.28, -123.12],
  "mexico city": [19.43, -99.13],
  "sao paulo": [-23.55, -46.63],
  santiago: [-33.45, -70.67],
  "buenos aires": [-34.6, -58.38],
  bogota: [4.71, -74.07],
  singapore: [1.35, 103.82],
  tokyo: [35.68, 139.69],
  osaka: [34.69, 135.5],
  seoul: [37.57, 126.98],
  "hong kong": [22.32, 114.17],
  taipei: [25.03, 121.57],
  mumbai: [19.08, 72.88],
  bangalore: [12.97, 77.59],
  jakarta: [-6.21, 106.85],
  sydney: [-33.87, 151.21],
  melbourne: [-37.81, 144.96],
  dubai: [25.2, 55.27],
  "tel aviv": [32.09, 34.78],
  johannesburg: [-26.2, 28.05],
};

/** The city a country's location is placed at when the panel names none. */
const COUNTRY_CITY: Record<string, string> = {
  FR: "gravelines",
  US: "dallas",
  DE: "frankfurt",
  NL: "amsterdam",
  GB: "london",
  IE: "dublin",
  BE: "brussels",
  LU: "luxembourg",
  CH: "zurich",
  AT: "vienna",
  IT: "milan",
  ES: "madrid",
  PT: "lisbon",
  PL: "warsaw",
  CZ: "prague",
  RO: "bucharest",
  BG: "sofia",
  FI: "helsinki",
  SE: "stockholm",
  NO: "oslo",
  DK: "copenhagen",
  TR: "istanbul",
  RU: "moscow",
  UA: "kyiv",
  CA: "montreal",
  MX: "mexico city",
  BR: "sao paulo",
  CL: "santiago",
  AR: "buenos aires",
  CO: "bogota",
  SG: "singapore",
  JP: "tokyo",
  KR: "seoul",
  HK: "hong kong",
  TW: "taipei",
  IN: "mumbai",
  ID: "jakarta",
  AU: "sydney",
  AE: "dubai",
  IL: "tel aviv",
  ZA: "johannesburg",
};

/** "São Paulo, BR" → "sao paulo". */
function cityKey(city: string): string {
  return city
    .split(",")[0]
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/[^a-zA-Z ]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .toLowerCase();
}

function titleCase(text: string): string {
  return text.replace(/\b[a-z]/g, (c) => c.toUpperCase());
}

export interface Place {
  /** Position on the map, in worldmap.ts units. */
  x: number;
  y: number;
  /** The city's name for the label, e.g. "Gravelines". */
  city: string;
}

/**
 * Where a location is on the map: the city the panel names, or the usual
 * city for its country. Null when neither is known, so it gets no pin.
 */
export function placeOf(location: { city?: string; country: string }): Place | null {
  const named = location.city ? cityKey(location.city) : "";
  const key = CITIES[named] ? named : COUNTRY_CITY[(location.country || "").toUpperCase()];
  const at = key ? CITIES[key] : undefined;
  if (!at) return null;
  const [x, y] = project(at[0], at[1]);
  return { x, y, city: CITIES[named] && location.city ? location.city.split(",")[0].trim() : titleCase(key) };
}
