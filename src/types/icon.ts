export interface IconMetadata {
  name: string;
  displayName: string;
  category: string;
  keywords: string[];
  defaultColor?: string;
}

export interface IconPreset {
  [key: string]: IconMetadata;
}
