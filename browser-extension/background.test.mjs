import test from 'node:test';
import assert from 'node:assert/strict';
import {matches, handle} from './background.mjs';

test('media title matching preserves Czech text and accepts the site suffix', () => {
  assert.equal(matches({title:'České video - YouTube'}, {title:'České video'}), true);
  assert.equal(matches({title:'Jiné video - YouTube'}, {title:'České video'}), false);
  assert.equal(matches({title:'Cizí České video'}, {title:'České video'}), false);
  assert.equal(matches({title:'Anything'}, {title:''}), false);
});
test('URL identity takes precedence over titles and preserves the YouTube video id', () => {
  assert.equal(matches({url:'https://www.youtube.com/watch?v=abc&t=10',title:'Changed'}, {url:'https://www.youtube.com/watch?v=abc',title:'Old'}), true);
  assert.equal(matches({url:'https://www.youtube.com/watch?v=other',title:'Old'}, {url:'https://www.youtube.com/watch?v=abc',title:'Old'}), false);
});
test('find only reports matches; activation selects an existing tab without navigation', async () => {
  const calls=[];
  const tab={id:7,windowId:2,title:'Video - YouTube',url:'https://www.youtube.com/watch?v=abc',audible:true};
  const api={tabs:{query:async()=>[tab,{...tab,id:8,title:'Other'}],get:async()=>tab,update:async(...args)=>calls.push(['tab',...args])},windows:{update:async(...args)=>calls.push(['window',...args])}};
  assert.equal((await handle(api,{op:'find',title:'Video',url:''})).tabs.length,1);
  assert.deepEqual(calls,[]);
  const reply=await handle(api,{op:'activate',tab_id:7,title:'Video',url:''});
  assert.equal(reply.tab.id,7);
  assert.deepEqual(calls,[['tab',7,{active:true}],['window',2,{focused:true}]]);
});
test('stale tabs are rejected before any activation', async () => {
  let activated=false;
  const api={tabs:{get:async()=>({id:7,title:'Unrelated'}),update:async()=>{activated=true;}}};
  await assert.rejects(handle(api,{op:'activate',tab_id:7,title:'Video',url:''}));
  assert.equal(activated,false);
});
