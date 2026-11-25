import assert from 'node:assert/strict';
import {test} from 'node:test';
import {AudioEpoch} from './audioEpoch.ts';

test('late ASR from another learner cannot update the new input',()=>{
 const guard=new AudioEpoch();guard.setSession('learner-A-session');const asr=guard.capture();
 guard.setSession('learner-B-session');assert.equal(guard.isCurrent(asr),false);
 guard.setSession('learner-A-session');assert.equal(guard.isCurrent(asr),false);
});
test('stop or a newer local turn invalidates pending playback and transcription',()=>{
 const guard=new AudioEpoch();guard.setSession('session');const pending=guard.capture();
 guard.invalidate();assert.equal(guard.isCurrent(pending),false);assert.equal(guard.isCurrent(guard.capture()),true);
});
test('ordinary polling of the same session preserves an active recording',()=>{
 const guard=new AudioEpoch();guard.setSession('session');const recording=guard.capture();
 guard.setSession('session');assert.equal(guard.isCurrent(recording),true);
});
