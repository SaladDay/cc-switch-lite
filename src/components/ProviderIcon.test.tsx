import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ProviderIcon } from "./ProviderIcon";

describe("ProviderIcon", () => {
  it("renders inline icons copied from the full provider catalog", () => {
    const { container } = render(
      <ProviderIcon icon="packycode" name="PackyCode" />,
    );

    expect(container.querySelector("svg")).not.toBeNull();
  });

  it("renders image-backed icons copied from the full provider catalog", () => {
    render(<ProviderIcon icon="apinebula" name="APINebula" />);

    expect(screen.getByTitle("APINebula")).toBeInstanceOf(HTMLImageElement);
  });
});
