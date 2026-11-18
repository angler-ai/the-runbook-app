// UI-4: an edge the builder can insert on. It draws as the usual smooth
// step edge; the builder's "+" sits just above the step the edge enters
// (one per step, however many edges join there).
import { BaseEdge, EdgeLabelRenderer, getSmoothStepPath, type EdgeProps } from '@xyflow/react';
import { useGraph } from './context';
import type { RunbookGraphEdge } from './layout';

export function InsertableEdge(props: EdgeProps<RunbookGraphEdge>) {
  const { builder } = useGraph();
  const [path, labelX, labelY] = getSmoothStepPath(props);
  const { data } = props;
  return (
    <>
      <BaseEdge
        id={props.id}
        path={path}
        {...(props.markerEnd ? { markerEnd: props.markerEnd } : {})}
        {...(props.style ? { style: props.style } : {})}
        {...(props.label !== undefined
          ? {
              label: props.label,
              labelX,
              labelY,
              ...(props.labelStyle ? { labelStyle: props.labelStyle } : {}),
              labelShowBg: true,
              ...(props.labelBgStyle ? { labelBgStyle: props.labelBgStyle } : {}),
              ...(props.labelBgPadding ? { labelBgPadding: props.labelBgPadding } : {}),
              ...(props.labelBgBorderRadius !== undefined
                ? { labelBgBorderRadius: props.labelBgBorderRadius }
                : {}),
            }
          : {})}
      />
      {builder && data?.insert && data.showInsert ? (
        <EdgeLabelRenderer>
          <div
            data-slot="edge-insert"
            className="nodrag nopan pointer-events-auto absolute"
            style={{
              transform: `translate(-50%, -50%) translate(${props.targetX}px, ${props.targetY - 16}px)`,
            }}
          >
            {builder.renderInsert(data.insert, data.insertLabel ?? 'Insert a step here')}
          </div>
        </EdgeLabelRenderer>
      ) : null}
    </>
  );
}
