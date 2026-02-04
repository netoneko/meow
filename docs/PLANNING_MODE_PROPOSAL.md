# Planning Mode Proposal

## Overview

Current implementation mixes planning (understanding the user request, breaking it down into tasks) and building (executing those tasks). This proposal outlines a separation of concerns to improve user experience, reliability, and maintainability.

## Current Challenges

*   **Complex Requests:** Difficult to handle complex, multi-step requests effectively.
*   **Dependency Management:** No explicit tracking of task dependencies.
*   **Error Handling:** Difficult to recover from errors mid-task.
*   **User Experience:**  Less transparent – user doesn’t see the breakdown of work.

## Proposed Solution

Introduce a dedicated "Planning Mode" that operates *before* the "Build Mode".

### Planning Mode Process

1.  **Request Analysis:** The LLM receives the user request.
2.  **Task Decomposition:** The LLM breaks down the request into a series of smaller, manageable tasks using the Chainlink issue tracker.
    *   Each task should have a clear title, description, and priority (low, medium, high).
    *   Dependencies between tasks should be identified and documented (e.g., task A must complete before task B).
3.  **Task List Review (Optional):** The user can review and modify the proposed task list before proceeding.
4.  **Planning Mode Exit:** Once the task list is finalized, Planning Mode transitions to Build Mode.

### Build Mode Process

1.  **Task Execution:** The LLM iterates through the task list, executing each task using the available tools.
2.  **Progress Tracking:** The LLM provides feedback on the progress of each task.
3.  **Error Handling:** If a task fails, Build Mode attempts to recover or notifies the user.

## Chainlink Integration

Chainlink will be the core of Planning Mode. We will use the following tools:

*   `ChainlinkInit`: Initialize the issue tracker database.
*   `ChainlinkCreate`: Create tasks.
*   `ChainlinkList`: Review task list.
*   `ChainlinkClose`/`ChainlinkReopen`: Manage task status.

## User Experience Improvements

*   **Transparency:** The user can see the breakdown of their request into individual tasks.
*   **Control:** The user can review and modify the task list.
*   **Reliability:** Explicit task dependencies and error handling improve the overall robustness of the system.

## Implementation Steps

1.  Refactor code to separate Planning Mode logic from Build Mode logic.
2.  Implement Chainlink integration for task management.
3.  Update the system prompt to guide the LLM through the Planning Mode process.
4.  Add a user interface element (e.g., a command or button) to initiate Planning Mode.

## Future Considerations

*   **Automated Dependency Detection:** Improve the LLM's ability to automatically identify task dependencies.
*   **Prioritization Algorithms:** Implement more sophisticated task prioritization algorithms.
*   **Resource Estimation:** Estimate the resources (e.g., time, cost) required to complete each task.
