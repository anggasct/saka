import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { decision, WORK_ITEM_ID } from "@/test/decision-fixtures";
import { renderWithProviders } from "@/test/render";

import { DecisionSection } from "./decision-section";
import type { DecisionRequest } from "./queries";

const LIST = "/api/v1/decision-requests";
const RUN = { kind: "work_item", id: WORK_ITEM_ID } as const;

function routes(items: () => DecisionRequest[], extra: Routes = {}): Routes {
  return { [LIST]: () => ({ body: page(items()) }), ...extra };
}

function answerPath(d: DecisionRequest): string {
  return `${LIST}/${d.id}/answer`;
}

describe("decision section", () => {
  it("renders nothing for a run that asked nothing", async () => {
    const { calls } = fakeApi(routes(() => []));
    renderWithProviders(<DecisionSection run={RUN} />);
    await waitFor(() => {
      expect(calls).toHaveLength(1);
    });
    expect(screen.queryByRole("region")).not.toBeInTheDocument();
  });

  it("asks for this run's questions only", async () => {
    const { requests } = fakeApi(routes(() => [decision(1)]));
    renderWithProviders(<DecisionSection run={RUN} />);
    await screen.findByRole("heading", { name: "Where do we store audio?" });
    expect(requests[0]?.search.get("work_item_id")).toBe(WORK_ITEM_ID);
    expect(requests[0]?.search.get("spec_draft_id")).toBeNull();
  });

  it("asks a draft's questions by the draft", async () => {
    const { requests } = fakeApi(routes(() => [decision(1)]));
    renderWithProviders(
      <DecisionSection run={{ kind: "spec_draft", id: "draft-1" }} />,
    );
    await screen.findByRole("heading", { name: "Where do we store audio?" });
    expect(requests[0]?.search.get("spec_draft_id")).toBe("draft-1");
    expect(requests[0]?.search.get("work_item_id")).toBeNull();
  });

  it("marks the first option as the recommendation and keeps send off until a choice or a note", async () => {
    fakeApi(routes(() => [decision(1)]));
    renderWithProviders(<DecisionSection run={RUN} />);
    const group = await screen.findByRole("group", { name: "Your choice" });
    const radios = within(group).getAllByRole("radio");
    expect(
      radios.map((r) => (r as HTMLInputElement).labels?.[0]?.textContent),
    ).toEqual(["Local diskRecommendedSimple, no cost", "R2"]);
    expect(within(group).getAllByText("Recommended")).toHaveLength(1);
    const send = screen.getByRole("button", { name: "Send answer" });
    expect(send).toBeDisabled();
    await userEvent.type(
      screen.getByLabelText("Note for the agent (optional)"),
      "either",
    );
    expect(send).toBeEnabled();
  });

  it("sends the chosen option and the note with an idempotency key", async () => {
    const asked = decision(1);
    const { requests } = fakeApi(
      routes(() => [asked], { [answerPath(asked)]: { body: asked } }),
    );
    renderWithProviders(<DecisionSection run={RUN} />);
    await userEvent.click(await screen.findByRole("radio", { name: /^R2/ }));
    await userEvent.type(
      screen.getByLabelText("Note for the agent (optional)"),
      "  because cost  ",
    );
    await userEvent.click(screen.getByRole("button", { name: "Send answer" }));
    await waitFor(() => {
      expect(requests.filter((r) => r.method === "POST")).toHaveLength(1);
    });
    const post = requests.find((r) => r.method === "POST");
    expect(post?.body).toEqual({ option: 1, note: "because cost" });
    expect(post?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("sends a note alone", async () => {
    const asked = decision(1);
    const { requests } = fakeApi(
      routes(() => [asked], { [answerPath(asked)]: { body: asked } }),
    );
    renderWithProviders(<DecisionSection run={RUN} />);
    await userEvent.type(
      await screen.findByLabelText("Note for the agent (optional)"),
      "either is fine",
    );
    await userEvent.click(screen.getByRole("button", { name: "Send answer" }));
    await waitFor(() => {
      expect(requests.some((r) => r.method === "POST")).toBe(true);
    });
    expect(requests.find((r) => r.method === "POST")?.body).toEqual({
      option: null,
      note: "either is fine",
    });
  });

  it("reloads the list when the question was answered elsewhere", async () => {
    const asked = decision(1);
    let answered = false;
    const { requests } = fakeApi(
      routes(
        () => [
          answered
            ? decision(1, {
                state: "answered",
                answer: {
                  option: 0,
                  note: null,
                  answered_by: "operator:local",
                  answered_at: "2026-11-06T11:00:00.000Z",
                },
              })
            : asked,
        ],
        {
          [answerPath(asked)]: () => {
            answered = true;
            return {
              status: 409,
              body: {
                type: "urn:saka:problem:decision_already_answered",
                title: "decision_already_answered",
                status: 409,
                code: "decision_already_answered",
                params: {},
                correlation_id: null,
              },
            };
          },
        },
      ),
    );
    renderWithProviders(<DecisionSection run={RUN} />);
    await userEvent.click(await screen.findByRole("radio", { name: /^R2/ }));
    await userEvent.click(screen.getByRole("button", { name: "Send answer" }));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Send answer" })).toBeNull();
    });
    expect(screen.getByText("Option 1: Local disk")).toBeInTheDocument();
    expect(
      requests.filter((r) => r.method === "GET").length,
    ).toBeGreaterThanOrEqual(2);
  });

  it("says why an answer was refused and keeps the form", async () => {
    const asked = decision(1);
    fakeApi(
      routes(() => [asked], {
        [answerPath(asked)]: {
          status: 422,
          body: {
            type: "urn:saka:problem:decision_answer_invalid",
            title: "decision_answer_invalid",
            status: 422,
            code: "decision_answer_invalid",
            params: {},
            correlation_id: null,
          },
        },
      }),
    );
    renderWithProviders(<DecisionSection run={RUN} />);
    await userEvent.click(await screen.findByRole("radio", { name: /^R2/ }));
    await userEvent.click(screen.getByRole("button", { name: "Send answer" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Choose one of the options or write a note.",
    );
    expect(screen.getByRole("button", { name: "Send answer" })).toBeEnabled();
  });

  it("shows a closed question with the answer and no form", async () => {
    fakeApi(
      routes(() => [
        decision(1, {
          state: "delivered",
          answer: {
            option: 1,
            note: "R2 is cheaper",
            answered_by: "operator:local",
            answered_at: "2026-11-06T11:00:00.000Z",
          },
        }),
        decision(2, { state: "expired", question: "Which region?" }),
      ]),
    );
    renderWithProviders(<DecisionSection run={RUN} />);
    const first = await screen.findByRole("article", {
      name: "Where do we store audio?",
    });
    expect(within(first).getByText("Sent to the agent")).toBeInTheDocument();
    expect(within(first).getByText("Option 2: R2")).toBeInTheDocument();
    expect(within(first).getByText("R2 is cheaper")).toBeInTheDocument();
    const second = screen.getByRole("article", { name: "Which region?" });
    expect(within(second).getByText("Expired")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Send answer" })).toBeNull();
  });

  it("speaks Indonesian", async () => {
    fakeApi(routes(() => [decision(1)]));
    renderWithProviders(<DecisionSection run={RUN} />, "id");
    expect(
      await screen.findByRole("heading", { name: "Pertanyaan dari agent" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("group", { name: "Pilihanmu" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Disarankan")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Kirim jawaban" }),
    ).toBeInTheDocument();
  });
});
