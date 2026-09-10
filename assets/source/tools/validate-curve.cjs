// Checks the enclosure curve surface.cjs draws: closed, within a hundredth
// of a pixel of the analytic superellipse, and tangent-continuous at every
// join. Writes the measurements to source/masters/curve-validation.json.
const assert=require('assert'),fs=require('fs'),path=require('path');
const {continuousPath,exponent}=require('./surface.cjs');
const p=continuousPath(100,100,824),v=p.match(/-?\d+(?:\.\d+)?/g).map(Number);
let start=v.slice(0,2),previous=start,maxError=0,maxJoinAngle=0,previousEnd;
let firstTangent;
for(let i=2;i<v.length;i+=6){
 const a=v.slice(i,i+2),b=v.slice(i+2,i+4),end=v.slice(i+4,i+6);
 const tangent=a.map((x,k)=>x-previous[k]);if(!firstTangent)firstTangent=tangent;
 function angle(u,w){return Math.acos(Math.min(1,Math.max(-1,(u[0]*w[0]+u[1]*w[1])/(Math.hypot(...u)*Math.hypot(...w)))));}
 if(previousEnd)maxJoinAngle=Math.max(maxJoinAngle,angle(previousEnd,tangent));
 for(let j=0;j<=20;j++){const t=j/20,z=1-t,q=previous.map((x,k)=>z**3*x+3*z*z*t*a[k]+3*z*t*t*b[k]+t**3*end[k]);const radial=Math.pow(Math.abs((q[0]-512)/412)**exponent+Math.abs((q[1]-512)/412)**exponent,1/exponent);maxError=Math.max(maxError,Math.abs(radial-1)*412);}
 previousEnd=end.map((x,k)=>x-b[k]);previous=end;
}
assert(Math.hypot(previous[0]-start[0],previous[1]-start[1])<1e-6);
assert(maxError<.01,'Curve differs from analytic reference');
assert(maxJoinAngle<1e-5,'Visible tangent discontinuity');
const result={shape:'Lame superellipse',exponent,segments:128,max_radial_error_px:maxError,max_internal_join_angle_radians:maxJoinAngle,closed:true,official_apple_mask:false};
fs.writeFileSync(path.join(__dirname,'../masters/curve-validation.json'),JSON.stringify(result,null,2)+'\n');console.log(result);
