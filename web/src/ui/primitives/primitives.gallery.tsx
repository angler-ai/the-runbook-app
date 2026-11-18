import { useId } from 'react';
import { defineGallery } from '../../gallery/registry';
import { Button } from './button';
import { Checkbox } from './checkbox';
import { Input } from './input';
import { Label } from './label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './select';
import { Tabs, TabsContent, TabsList, TabsTrigger } from './tabs';
import { Textarea } from './textarea';

function FieldsDemo() {
  const id = useId();
  return (
    <div className="grid max-w-sm gap-3">
      <div className="grid gap-1.5">
        <Label htmlFor={`${id}-service`}>Service</Label>
        <Input id={`${id}-service`} placeholder="api" />
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor={`${id}-region`}>Region</Label>
        <Select>
          <SelectTrigger id={`${id}-region`}>
            <SelectValue placeholder="Choose a region" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="east">east</SelectItem>
            <SelectItem value="west">west</SelectItem>
          </SelectContent>
        </Select>
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor={`${id}-summary`}>Summary</Label>
        <Textarea id={`${id}-summary`} placeholder="What did you find?" />
      </div>
      <div className="flex items-center gap-3">
        <Checkbox id={`${id}-check`} />
        <Label htmlFor={`${id}-check`}>Smoke test passed</Label>
      </div>
    </div>
  );
}

export default [
  defineGallery({
    id: 'primitives-button',
    group: 'Primitives',
    title: 'Button',
    examples: [
      {
        name: 'Variants',
        render: () => (
          <div className="flex flex-wrap gap-2">
            <Button>Start run</Button>
            <Button variant="secondary">Cancel</Button>
            <Button variant="ghost">More</Button>
            <Button variant="destructive">Roll back</Button>
            <Button variant="link">View report</Button>
            <Button disabled>Disabled</Button>
          </div>
        ),
      },
    ],
  }),
  defineGallery({
    id: 'primitives-fields',
    group: 'Primitives',
    title: 'Form fields',
    examples: [{ name: 'Label, input, select, textarea, checkbox', render: () => <FieldsDemo /> }],
  }),
  defineGallery({
    id: 'primitives-tabs',
    group: 'Primitives',
    title: 'Tabs',
    examples: [
      {
        name: 'Editor tabs',
        render: () => (
          <Tabs defaultValue="yaml">
            <TabsList>
              <TabsTrigger value="yaml">YAML</TabsTrigger>
              <TabsTrigger value="graph">Graph</TabsTrigger>
              <TabsTrigger value="outline">Outline</TabsTrigger>
            </TabsList>
            <TabsContent value="yaml">YAML view</TabsContent>
            <TabsContent value="graph">Graph view</TabsContent>
            <TabsContent value="outline">Outline view</TabsContent>
          </Tabs>
        ),
      },
    ],
  }),
];
