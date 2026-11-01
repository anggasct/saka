const MAX_LINES = 3;
const TASK_MARKER = /\b(TODO|FIXME|XXX|HACK)\b/;
const DIRECTIVE = /^(\/ <reference |\s*@vitest-environment )/;

export const comments = {
  meta: {
    type: "suggestion",
    schema: [],
    messages: {
      block:
        "Block and doc comments are not used; write a short // why comment only for complex logic.",
      long: "A comment is at most 3 lines; move the rest into names, tests, or the docs.",
      task: "Task markers are not committed; finish the work or track it outside the code.",
      code: "Commented-out code is deleted; git keeps the history.",
    },
  },
  create(context) {
    return {
      Program() {
        let run = { last: -1, count: 0 };
        for (const comment of context.sourceCode.getAllComments()) {
          if (comment.type === "Shebang" || DIRECTIVE.test(comment.value)) {
            continue;
          }
          if (comment.type === "Block" || comment.value.startsWith("/")) {
            context.report({ loc: comment.loc, messageId: "block" });
            continue;
          }
          if (TASK_MARKER.test(comment.value)) {
            context.report({ loc: comment.loc, messageId: "task" });
          }
          if (/[;{}]\s*$/.test(comment.value)) {
            context.report({ loc: comment.loc, messageId: "code" });
          }
          const line = comment.loc.start.line;
          run =
            line === run.last + 1
              ? { last: line, count: run.count + 1 }
              : { last: line, count: 1 };
          if (run.count === MAX_LINES + 1) {
            context.report({ loc: comment.loc, messageId: "long" });
          }
        }
      },
    };
  },
};
